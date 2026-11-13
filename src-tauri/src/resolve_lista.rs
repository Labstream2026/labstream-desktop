//! La lista de bibliotecas del Resolve de ESTE equipo (1.30, 17-oct-2026).
//!
//! Resolve guarda en cada equipo las bibliotecas de red a las que entra (dblist.conf), una línea por
//! biblioteca: `<base><ip>:<ip>:<usuario>:<clave>:<base>:QPSQL`. Cuando nacía una biblioteca nueva,
//! alguien tenía que ir equipo por equipo a añadirla. Ahora la página del OS, dentro de esta app, lo
//! hace sola en tres pasos (Labstream OS, lib/resolve-bibliotecas/lista-equipo):
//!
//!   1. `resolve_lista`: qué usuarios de PostgreSQL y qué direcciones usa la lista, y qué bibliotecas
//!      tiene ya. NUNCA la clave: no sale de este equipo.
//!   2. La página le pregunta al OS a cuáles tiene acceso cada usuario.
//!   3. `resolve_lista_agregar`: añade las que falten copiando usuario, clave y dirección de una línea
//!      que ya funciona. Solo con Resolve CERRADO (al cerrarse reescribe su lista y se comería lo
//!      añadido) y con una copia de la lista antes de tocarla. Solo AÑADE al final: las líneas que
//!      ya estaban (también la base local «DISK») no se tocan.
//!
//! `resolve_lista_renombrar` (1.34, 4-nov-2026): una biblioteca que cambió de nombre en el servidor
//! dejaba aquí una línea muerta, y si era la marcada Resolve se quedaba sin entrar. Reescribe esa línea
//! con el nombre nuevo: la dirección, el usuario, la clave y la marca no cambian. Solo toca esas líneas,
//! con las mismas reglas: Resolve cerrado y una copia antes.
//!
//! `vistas` (resolve-lista.json en la configuración de la app): lo que ya se vio alguna vez en la
//! lista o se añadió. Si alguien quita una biblioteca a propósito desde Resolve, no se le vuelve a poner.
//!
//! `modo` (1.40, resolve-modo.json): qué hace la app con Resolve en ESTE equipo. Automático (la oficina
//! si responde, si no ZeroTier), Local u ZeroTier elegidos a mano, o Apagado: en un equipo sin Resolve
//! la app no mira ni toca la lista. Se elige en el menú ⋮ de la app o en la pestaña Resolve del OS.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime};

/// Dos pestañas pueden pedir a la vez: se añade de una en una (y cada una vuelve a mirar la lista).
static CANDADO: Mutex<()> = Mutex::new(());
const COPIAS_QUE_SE_GUARDAN: usize = 5;
const PREFIJO_COPIA: &str = "dblist.conf.antes-labstream-";

// ── Lo puro: leer y escribir líneas ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Linea {
    pub ip: String,
    pub rol: String,
    pub clave: String,
    pub base: String,
}

/// Una línea de biblioteca de red, o None (la base local, una línea en blanco o rota).
/// Resolve marca con «*» tras el usuario la última biblioteca abierta («camilo*»): es el mismo usuario
/// (1.30.1). Se le quita para contarlo y buscar su clave; las líneas nuevas nunca llevan la marca.
pub(crate) fn leer_linea(l: &str) -> Option<Linea> {
    let l = l.trim_end_matches(['\r', '\n']);
    let c: Vec<&str> = l.split(':').collect();
    if c.len() != 6 {
        return None;
    }
    let rol = c[2].trim_end_matches('*');
    if c[5] != "QPSQL" || c[1].is_empty() || rol.is_empty() || c[4].is_empty() {
        return None;
    }
    Some(Linea { ip: c[1].into(), rol: rol.into(), clave: c[3].into(), base: c[4].into() })
}

/// La misma línea que escribe el panel del NAS (cnLinea): `<base><ip>:<ip>:<usuario>:<clave>:<base>:QPSQL`.
pub(crate) fn escribir_linea(base: &str, ip: &str, rol: &str, clave: &str) -> String {
    format!("{base}{ip}:{ip}:{rol}:{clave}:{base}:QPSQL")
}

/// El nombre que acepta el panel para una base (sin «:» ni saltos), menos las reservadas.
pub(crate) fn nombre_valido(n: &str) -> bool {
    let reservada = ["postgres", "template0", "template1", "_plantilla", "plantilla"].contains(&n.to_ascii_lowercase().as_str());
    !reservada && (1..=60).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Cuenta {
    rol: String,
    ip: String,
    lineas: u32,
}

/// Usuario y dirección de cada línea (sin claves), en el orden en que aparecen.
pub(crate) fn cuentas_de(lineas: &[Linea]) -> Vec<Cuenta> {
    let mut out: Vec<Cuenta> = Vec::new();
    for l in lineas {
        match out.iter_mut().find(|c| c.rol == l.rol && c.ip == l.ip) {
            Some(c) => c.lineas += 1,
            None => out.push(Cuenta { rol: l.rol.clone(), ip: l.ip.clone(), lineas: 1 }),
        }
    }
    out
}

/// La clave de ese usuario en esa dirección: la que más se repite (si alguien cambió la clave y solo
/// arregló una línea, gana la de la mayoría; a igualdad, la más reciente, que es la de más abajo).
pub(crate) fn clave_de(lineas: &[Linea], rol: &str, ip: &str) -> Option<String> {
    let mut cuenta: Vec<(String, usize, usize)> = Vec::new(); // (clave, veces, última posición)
    for (i, l) in lineas.iter().enumerate().filter(|(_, l)| l.rol == rol && l.ip == ip) {
        match cuenta.iter_mut().find(|c| c.0 == l.clave) {
            Some(c) => {
                c.1 += 1;
                c.2 = i;
            }
            None => cuenta.push((l.clave.clone(), 1, i)),
        }
    }
    cuenta.into_iter().max_by_key(|c| (c.1, c.2)).map(|c| c.0)
}

/// Lo que hay que escribir al final del archivo para añadir `nuevas` (con el salto que use el archivo).
pub(crate) fn texto_a_anadir(actual: &str, nuevas: &[String]) -> String {
    let salto = if actual.contains("\r\n") || (actual.is_empty() && cfg!(windows)) { "\r\n" } else { "\n" };
    let mut s = String::new();
    if !actual.is_empty() && !actual.ends_with('\n') {
        s.push_str(salto);
    }
    for l in nuevas {
        s.push_str(l);
        s.push_str(salto);
    }
    s
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Renombre {
    de: String,
    a: String,
}

/// La misma línea con otro nombre de base. Dirección, usuario, clave y la marca «*» se quedan.
fn linea_con_otro_nombre(l: &str, de: &str, a: &str) -> String {
    let cuerpo = l.trim_end_matches(['\r', '\n']);
    let fin = &l[cuerpo.len()..];
    let mut c: Vec<String> = cuerpo.split(':').map(String::from).collect();
    if let Some(resto) = c[0].strip_prefix(de) {
        c[0] = format!("{a}{resto}");
    }
    c[4] = a.to_string();
    format!("{}{fin}", c.join(":"))
}

/// El texto de la lista con los cambios de nombre, y cuáles aplicó. Van en orden: A → B → C. Si la
/// nueva ya estaba (la app anterior la añadió como una más), la vieja sobra y se quita. Todo lo demás
/// sale idéntico, con sus saltos de línea.
pub(crate) fn cambiar_nombres(actual: &str, renombres: &[Renombre]) -> (String, Vec<Renombre>) {
    let mut lineas: Vec<String> = actual.split_inclusive('\n').map(String::from).collect();
    let mut hechos: Vec<Renombre> = Vec::new();
    for r in renombres.iter().take(50) {
        if !nombre_valido(&r.de) || !nombre_valido(&r.a) {
            continue;
        }
        let bases: BTreeSet<String> = lineas.iter().filter_map(|l| leer_linea(l)).map(|l| l.base).collect();
        if !bases.contains(&r.de) {
            continue;
        }
        let ya_esta = bases.contains(&r.a);
        lineas = lineas
            .into_iter()
            .filter_map(|l| match leer_linea(&l) {
                Some(x) if x.base == r.de => (!ya_esta).then(|| linea_con_otro_nombre(&l, &r.de, &r.a)),
                _ => Some(l),
            })
            .collect();
        hechos.push(r.clone());
    }
    (lineas.concat(), hechos)
}

// ── Dónde está la lista y si Resolve está abierto ────────────────────────────────────────────

/// Dónde puede estar dblist.conf en este sistema: los sitios fijos, por orden de preferencia, y las
/// carpetas de Blackmagic donde buscarlo si no está en ninguno.
///
/// En Windows está con las preferencias de Resolve (%APPDATA%\…\Preferences). En Mac las preferencias
/// de Resolve viven en ~/Library/Preferences/Blackmagic Design/DaVinci Resolve, NO en Application
/// Support: hasta la 1.40.1 solo se miraba Application Support/…/Preferences, y en los Mac la app decía
/// «Resolve no tiene bibliotecas de red en este equipo» aunque las tuviera (12-nov-2026, el Mac de
/// Munka: ni le llegaban las bibliotecas nuevas ni cambiaba de red). Por si otra versión de Resolve lo
/// guarda en otro sitio, si no está en los fijos se busca bajo las carpetas de Blackmagic.
fn donde_buscar() -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    #[cfg(windows)]
    {
        let a = PathBuf::from(std::env::var_os("APPDATA")?);
        Some((vec![a.join("Blackmagic Design").join("DaVinci Resolve").join("Preferences").join("dblist.conf")], vec![]))
    }
    #[cfg(target_os = "macos")]
    {
        let h = PathBuf::from(std::env::var_os("HOME")?);
        Some((
            vec![
                h.join("Library/Preferences/Blackmagic Design/DaVinci Resolve/dblist.conf"),
                h.join("Library/Application Support/Blackmagic Design/DaVinci Resolve/Preferences/dblist.conf"),
                h.join("Library/Application Support/Blackmagic Design/DaVinci Resolve/dblist.conf"),
            ],
            vec![
                h.join("Library/Preferences/Blackmagic Design"),
                h.join("Library/Application Support/Blackmagic Design"),
                PathBuf::from("/Library/Application Support/Blackmagic Design"),
                // El Resolve de la App Store guarda lo suyo dentro de su contenedor.
                h.join("Library/Containers/com.blackmagic-design.DaVinciResolveAppStore/Data/Library"),
                h.join("Library/Containers/com.blackmagic-design.DaVinciResolveLite/Data/Library"),
            ],
        ))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

const FONDO_DE_BUSQUEDA: usize = 5;
const TOPE_DE_CARPETAS: usize = 4000;
/// Árboles grandes de Resolve donde la lista no está: no se entra (proyectos, LUT, Fusion, registros).
const NO_ENTRAR: [&str; 6] = ["Resolve Project Library", "Resolve Disk Database", "Fusion", "LUT", "logs", "CacheClip"];

/// Los dblist.conf que hay bajo `raiz`, sin bajar más de `fondo` carpetas ni seguir enlaces.
pub(crate) fn buscar_lista(raiz: &Path, fondo: usize) -> Vec<PathBuf> {
    let mut hallados = Vec::new();
    let mut pila = vec![(raiz.to_path_buf(), 0usize)];
    let mut vistas = 0usize;
    while let Some((dir, nivel)) = pila.pop() {
        vistas += 1;
        if vistas > TOPE_DE_CARPETAS {
            break;
        }
        let Ok(entradas) = std::fs::read_dir(&dir) else { continue };
        for e in entradas.flatten() {
            let Ok(tipo) = e.file_type() else { continue };
            let nombre = e.file_name();
            if tipo.is_file() && nombre == "dblist.conf" {
                hallados.push(e.path());
            } else if tipo.is_dir() && nivel < fondo && !NO_ENTRAR.iter().any(|n| nombre == *n) {
                pila.push((e.path(), nivel + 1));
            }
        }
    }
    hallados.sort();
    hallados
}

/// De las candidatas, la primera que tiene bibliotecas de red; si ninguna las tiene, la primera que existe.
pub(crate) fn elegir_lista(candidatas: &[PathBuf]) -> Option<PathBuf> {
    let existen: Vec<&PathBuf> = candidatas.iter().filter(|p| p.is_file()).collect();
    existen
        .iter()
        .find(|p| std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| leer_linea(l).is_some())))
        .or(existen.first())
        .map(|p| (*p).clone())
}

/// La lista de este equipo. Si no existe en ningún sitio, el sitio principal (para decir «no está»).
fn ruta_lista() -> Option<PathBuf> {
    let (fijas, raices) = donde_buscar()?;
    if let Some(p) = elegir_lista(&fijas) {
        return Some(p);
    }
    let halladas: Vec<PathBuf> = raices.iter().flat_map(|r| buscar_lista(r, FONDO_DE_BUSQUEDA)).collect();
    elegir_lista(&halladas).or_else(|| fijas.into_iter().next())
}

/// Una ruta para leerla: la carpeta de la persona se dice «~».
fn legible(p: &Path) -> String {
    let casa = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    match casa.as_deref().and_then(|c| p.strip_prefix(c).ok()) {
        Some(resto) => format!("~{}{}", std::path::MAIN_SEPARATOR, resto.display()),
        None => p.display().to_string(),
    }
}

#[cfg(windows)]
fn resolve_abierto() -> bool {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
    };
    unsafe {
        // Si no se puede mirar, se da por ABIERTO: mejor esperar que comerse la lista.
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return true };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut abierto = false;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let fin = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..fin]).eq_ignore_ascii_case("Resolve.exe") {
                    abierto = true;
                    break;
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        abierto
    }
}

#[cfg(target_os = "macos")]
fn resolve_abierto() -> bool {
    // El ejecutable de «DaVinci Resolve.app» se llama Resolve. Si pgrep falla, se da por abierto.
    match std::process::Command::new("/usr/bin/pgrep").args(["-x", "Resolve"]).output() {
        Ok(o) => o.status.code() != Some(1),
        Err(_) => true,
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn resolve_abierto() -> bool {
    true
}

// ── Lo que ya se vio ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Vistas {
    #[serde(default)]
    vistas: BTreeSet<String>,
}

fn ruta_vistas<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("resolve-lista.json"))
}
fn leer_vistas(p: Option<&Path>) -> Vistas {
    p.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn guardar_vistas(p: Option<&Path>, v: &Vistas) {
    let Some(p) = p else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(v) {
        let _ = std::fs::write(p, s);
    }
}

// ── El modo de ESTE equipo (1.40, 12-nov-2026) ───────────────────────────────────────────────
//
// Munka: «quiero un botón que selecciones si estoy local o por ZeroTier o apagado para las personas
// que no tienen Resolve». Automático sigue siendo lo de la 1.39: la oficina si responde, si no
// ZeroTier. Local y ZeroTier fijan la dirección aunque la otra responda (por ejemplo, para dejarlo
// listo antes de salir de la oficina). Apagado: la app no mira la lista ni la toca, ni la red.

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Modo {
    #[default]
    Auto,
    Oficina,
    Zerotier,
    Apagado,
}

impl Modo {
    pub(crate) const TODOS: [Modo; 4] = [Modo::Auto, Modo::Oficina, Modo::Zerotier, Modo::Apagado];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Modo::Auto => "auto",
            Modo::Oficina => "oficina",
            Modo::Zerotier => "zerotier",
            Modo::Apagado => "apagado",
        }
    }

    pub(crate) fn de_id(s: &str) -> Option<Modo> {
        Modo::TODOS.into_iter().find(|m| m.id() == s)
    }

    /// Lo que dice el menú ⋮.
    pub(crate) fn etiqueta(self) -> &'static str {
        match self {
            Modo::Auto => "Automático (la oficina si responde)",
            Modo::Oficina => "Local (red de la oficina)",
            Modo::Zerotier => "ZeroTier (desde fuera)",
            Modo::Apagado => "Apagado (este equipo no usa Resolve)",
        }
    }
}

const APAGADO: &str = "Resolve está apagado en este equipo (menú ⋮ de la app, «Resolve en este equipo»).";

#[derive(Serialize, Deserialize, Default)]
struct ConfigModo {
    #[serde(default)]
    modo: Modo,
}

fn ruta_modo<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("resolve-modo.json"))
}

/// El modo guardado de este equipo; Automático si no hay nada o el archivo no se entiende.
pub(crate) fn modo<R: Runtime>(app: &AppHandle<R>) -> Modo {
    ruta_modo(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<ConfigModo>(&s).ok())
        .map(|c| c.modo)
        .unwrap_or_default()
}

pub(crate) fn guardar_modo<R: Runtime>(app: &AppHandle<R>, m: Modo) -> Result<(), String> {
    let p = ruta_modo(app).ok_or("No encuentro la carpeta de configuración de la app.")?;
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let txt = serde_json::to_string(&ConfigModo { modo: m }).map_err(|e| e.to_string())?;
    std::fs::write(&p, txt).map_err(|e| format!("No pude guardar el modo de Resolve: {e}"))
}

#[tauri::command]
pub fn resolve_modo<R: Runtime>(app: AppHandle<R>) -> Modo {
    modo(&app)
}

/// Guarda el modo y avisa a las pestañas (`ls-resolve-modo`) para que la del OS se ponga al día.
/// Aplicarlo a la lista lo hace quien llama (la página, o el menú ⋮ con `aplicar_modo`).
#[tauri::command]
pub fn resolve_modo_poner<R: Runtime>(app: AppHandle<R>, modo: Modo) -> Result<Modo, String> {
    guardar_modo(&app, modo)?;
    let _ = tauri::Emitter::emit(&app, "ls-resolve-modo", modo.id());
    Ok(modo)
}

// ── Los comandos ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct Estado {
    disponible: bool,
    motivo: Option<String>,
    abierto: bool,
    cuentas: Vec<Cuenta>,
    nombres: Vec<String>,
    vistas: Vec<String>,
}

const SIN_BIBLIOTECAS: &str = "Resolve no tiene bibliotecas de red en este equipo.";

/// «No tiene bibliotecas» con lo que se miró: en un Mac, si al menos están las preferencias de Resolve
/// (está instalado y se ha abierto) o ni eso. Se lee en la lista de equipos de la pestaña Resolve.
fn sin_bibliotecas_con_detalle() -> String {
    let Some((fijas, _)) = donde_buscar() else { return SIN_BIBLIOTECAS.into() };
    let carpeta = fijas.first().and_then(|p| p.parent()).is_some_and(|d| d.is_dir());
    format!(
        "Resolve no tiene bibliotecas de red en este equipo: no encontré su lista (dblist.conf). {}",
        if carpeta { "Las preferencias de Resolve sí están: añade una biblioteca de red a mano una vez y las demás llegan solas." } else { "Tampoco están las preferencias de Resolve: o no está instalado o no se ha abierto nunca con esta cuenta." }
    )
}

fn sin_lista(motivo: &str) -> Estado {
    Estado { disponible: false, motivo: Some(motivo.into()), abierto: false, cuentas: vec![], nombres: vec![], vistas: vec![] }
}

fn mirar(ruta: Option<PathBuf>, vistas_en: Option<&Path>, abierto: fn() -> bool) -> Estado {
    let Some(ruta) = ruta else { return sin_lista("Este sistema no tiene la lista de Resolve.") };
    let texto = match std::fs::read_to_string(&ruta) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return sin_lista(SIN_BIBLIOTECAS),
        Err(e) => return sin_lista(&format!("No pude leer la lista de Resolve: {e}")),
    };
    let lineas: Vec<Linea> = texto.lines().filter_map(leer_linea).collect();
    if lineas.is_empty() {
        return sin_lista("La lista de Resolve no tiene ninguna biblioteca de red de la que copiar el acceso.");
    }
    let nombres: Vec<String> = lineas.iter().map(|l| l.base.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    // Lo que está hoy en la lista queda «visto»: si mañana alguien la quita, no se vuelve a poner.
    let mut v = leer_vistas(vistas_en);
    let antes = v.vistas.len();
    v.vistas.extend(nombres.iter().cloned());
    if v.vistas.len() != antes {
        guardar_vistas(vistas_en, &v);
    }
    Estado { disponible: true, motivo: None, abierto: abierto(), cuentas: cuentas_de(&lineas), nombres, vistas: v.vistas.into_iter().collect() }
}

#[tauri::command]
pub async fn resolve_lista<R: Runtime>(app: AppHandle<R>) -> Estado {
    if modo(&app) == Modo::Apagado {
        return sin_lista(APAGADO);
    }
    let vistas = ruta_vistas(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        let mut e = mirar(ruta_lista(), vistas.as_deref(), resolve_abierto);
        if !e.disponible && e.motivo.as_deref() == Some(SIN_BIBLIOTECAS) {
            e.motivo = Some(sin_bibliotecas_con_detalle());
        }
        e
    })
    .await
    .unwrap_or_else(|_| sin_lista("No pude mirar la lista de Resolve."))
}

#[derive(Deserialize)]
pub struct Paso {
    rol: String,
    ip: String,
    bibliotecas: Vec<String>,
    /// A petición (1.40.3, «Volver a ponerlas» de la pestaña Resolve): también las que ya se vieron
    /// y alguien quitó a mano de este Resolve. La vuelta automática nunca lo manda.
    #[serde(default)]
    forzar: bool,
}

#[derive(Serialize)]
pub struct Agregado {
    agregadas: Vec<String>,
    copia: Option<String>,
}

/// Copia de la lista tal como estaba (se guardan las 5 últimas).
fn copiar_lista(ruta: &Path) -> Result<PathBuf, String> {
    let dir = ruta.parent().ok_or("La lista de Resolve no tiene carpeta.")?;
    let sello = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let copia = dir.join(format!("{PREFIJO_COPIA}{sello}"));
    std::fs::copy(ruta, &copia).map_err(|e| format!("No pude guardar la copia de la lista: {e}"))?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut copias: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIJO_COPIA))).collect();
        copias.sort();
        let sobran = copias.len().saturating_sub(COPIAS_QUE_SE_GUARDAN);
        for p in copias.into_iter().take(sobran) {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(copia)
}

fn agregar(ruta: Option<PathBuf>, vistas_en: Option<&Path>, paso: Paso, abierto: fn() -> bool) -> Result<Agregado, String> {
    let ruta = ruta.ok_or("Este sistema no tiene la lista de Resolve.")?;
    if abierto() {
        return Err("Resolve está abierto: se añaden cuando lo cierres.".into());
    }
    let actual = std::fs::read_to_string(&ruta).map_err(|e| format!("No pude leer la lista de Resolve: {e}"))?;
    let lineas: Vec<Linea> = actual.lines().filter_map(leer_linea).collect();
    // Usuario y dirección tienen que salir de una línea que YA está: la página no puede inventar
    // otro servidor ni otro usuario, y la clave la pone este equipo.
    let clave = clave_de(&lineas, &paso.rol, &paso.ip).ok_or("Ese usuario no está en la lista de Resolve de este equipo.")?;
    let mut v = leer_vistas(vistas_en);
    let ya: BTreeSet<&str> = lineas.iter().map(|l| l.base.as_str()).collect();
    let mut nuevas: Vec<String> = Vec::new();
    for b in paso.bibliotecas.iter().take(50) {
        if nombre_valido(b) && !ya.contains(b.as_str()) && (paso.forzar || !v.vistas.contains(b)) && !nuevas.contains(b) {
            nuevas.push(b.clone());
        }
    }
    if nuevas.is_empty() {
        return Ok(Agregado { agregadas: vec![], copia: None });
    }

    let copia = copiar_lista(&ruta)?;

    // Última comprobación justo antes de escribir: si Resolve se abrió mientras tanto, nada.
    if abierto() {
        return Err("Resolve está abierto: se añaden cuando lo cierres.".into());
    }
    let texto: Vec<String> = nuevas.iter().map(|b| escribir_linea(b, &paso.ip, &paso.rol, &clave)).collect();
    let mut f = std::fs::OpenOptions::new().append(true).open(&ruta).map_err(|e| format!("No pude abrir la lista de Resolve: {e}"))?;
    f.write_all(texto_a_anadir(&actual, &texto).as_bytes()).map_err(|e| format!("No pude escribir en la lista de Resolve: {e}"))?;
    let _ = f.sync_all();

    v.vistas.extend(nuevas.iter().cloned());
    guardar_vistas(vistas_en, &v);
    Ok(Agregado { agregadas: nuevas, copia: copia.file_name().and_then(|n| n.to_str()).map(String::from) })
}

#[tauri::command]
pub async fn resolve_lista_agregar<R: Runtime>(app: AppHandle<R>, paso: Paso) -> Result<Agregado, String> {
    if modo(&app) == Modo::Apagado {
        return Err(APAGADO.into());
    }
    let vistas = ruta_vistas(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        agregar(ruta_lista(), vistas.as_deref(), paso, resolve_abierto)
    })
    .await
    .map_err(|e| format!("No pude añadir las bibliotecas: {e}"))?
}

#[derive(Serialize)]
pub struct Renombrado {
    renombradas: Vec<String>,
    copia: Option<String>,
}

fn renombrar(ruta: Option<PathBuf>, vistas_en: Option<&Path>, renombres: &[Renombre], abierto: fn() -> bool) -> Result<Renombrado, String> {
    const ABIERTO: &str = "Resolve está abierto: se cambian cuando lo cierres.";
    let ruta = ruta.ok_or("Este sistema no tiene la lista de Resolve.")?;
    if abierto() {
        return Err(ABIERTO.into());
    }
    let actual = std::fs::read_to_string(&ruta).map_err(|e| format!("No pude leer la lista de Resolve: {e}"))?;
    let (texto, hechos) = cambiar_nombres(&actual, renombres);
    if hechos.is_empty() {
        return Ok(Renombrado { renombradas: vec![], copia: None });
    }
    let copia = copiar_lista(&ruta)?;
    // Última comprobación justo antes de escribir: si Resolve se abrió mientras tanto, nada.
    if abierto() {
        return Err(ABIERTO.into());
    }
    // Aparte y de un golpe: si esto se corta a medias, la lista sigue entera.
    let temporal = ruta.with_extension("labstream-nueva");
    std::fs::write(&temporal, &texto).map_err(|e| format!("No pude escribir en la lista de Resolve: {e}"))?;
    std::fs::rename(&temporal, &ruta).map_err(|e| format!("No pude escribir en la lista de Resolve: {e}"))?;

    // El nombre viejo deja de contar como visto: si un día vuelve, tiene que poder entrar.
    let mut v = leer_vistas(vistas_en);
    for r in &hechos {
        v.vistas.remove(&r.de);
        v.vistas.insert(r.a.clone());
    }
    guardar_vistas(vistas_en, &v);
    Ok(Renombrado { renombradas: hechos.into_iter().map(|r| r.a).collect(), copia: copia.file_name().and_then(|n| n.to_str()).map(String::from) })
}

#[tauri::command]
pub async fn resolve_lista_renombrar<R: Runtime>(app: AppHandle<R>, renombres: Vec<Renombre>) -> Result<Renombrado, String> {
    if modo(&app) == Modo::Apagado {
        return Err(APAGADO.into());
    }
    let vistas = ruta_vistas(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        renombrar(ruta_lista(), vistas.as_deref(), &renombres, resolve_abierto)
    })
    .await
    .map_err(|e| format!("No pude cambiar los nombres: {e}"))?
}

// ── Por dónde se llega al servidor: la oficina o ZeroTier (1.39, 12-nov-2026) ──────────────────
//
// Munka: «mi Mac a veces lo utilizo en local al llevarlo a la oficina y por ZeroTier desde la casa».
// Cada línea de la lista lleva la dirección del servidor. Un equipo con las líneas de la oficina no
// abría ninguna biblioteca desde casa, y las nuevas le entraban también con la dirección de oficina.
// Ahora la app mira por dónde llega al servidor y deja las líneas con ESA dirección:
//   · si responde la de la oficina, la oficina, AUNQUE ZeroTier también responda (Munka: «los equipos
//     que están locales y por ZeroTier solo se deben conectar por local, porque es más rápido»);
//   · si solo responde ZeroTier, ZeroTier;
//   · si no responde ninguna, no se toca nada.
// Es el mismo servidor (el NAS tiene las dos direcciones), así que usuario y clave valen igual. Las
// dos direcciones están escritas AQUÍ y no las manda la página: la clave de una línea no puede acabar
// apuntando a otro servidor. Las líneas de cualquier otra dirección no se tocan. Mismas reglas que
// siempre: Resolve cerrado y una copia antes.

/// El servidor de las bibliotecas: (en la oficina, por ZeroTier). Las mismas que `IPS` de carpetas_os.
const SERVIDOR: (&str, &str) = ("192.168.0.22", "10.61.5.96");
const PUERTO_POSTGRES: u16 = 5432;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Red {
    Oficina,
    Zerotier,
}

/// Por dónde entrar. La oficina gana siempre que responda.
pub(crate) fn red_de(oficina: bool, zerotier: bool) -> Option<Red> {
    if oficina {
        Some(Red::Oficina)
    } else if zerotier {
        Some(Red::Zerotier)
    } else {
        None
    }
}

/// La misma línea con otra dirección. Usuario (con su marca «*»), clave y base se quedan.
fn linea_con_otra_direccion(l: &str, de: &str, a: &str) -> String {
    let cuerpo = l.trim_end_matches(['\r', '\n']);
    let fin = &l[cuerpo.len()..];
    let mut c: Vec<String> = cuerpo.split(':').map(String::from).collect();
    if let Some(base) = c[0].strip_suffix(de) {
        c[0] = format!("{base}{a}");
    }
    c[1] = a.to_string();
    format!("{}{fin}", c.join(":"))
}

/// El texto de la lista con las líneas de la dirección `de` pasadas a `a`, y qué bibliotecas cambiaron.
/// Si una biblioteca ya tenía línea con `a` (para el mismo usuario), la de `de` sobra y se quita: cada
/// biblioteca sale una sola vez. Si la que sobra era la marcada como última abierta, la marca pasa a
/// la que se queda. Todo lo demás sale idéntico, con sus saltos de línea.
pub(crate) fn cambiar_direccion(actual: &str, de: &str, a: &str) -> (String, Vec<String>) {
    let lineas: Vec<&str> = actual.split_inclusive('\n').collect();
    let leidas: Vec<Option<Linea>> = lineas.iter().map(|l| leer_linea(l)).collect();
    let marcada = |l: &str| l.split(':').nth(2).is_some_and(|u| u.ends_with('*'));
    let gemela = |x: &Linea| leidas.iter().position(|y| y.as_ref().is_some_and(|y| y.ip == a && y.rol == x.rol && y.base == x.base));
    // Las gemelas que heredan la marca de una línea que se quita.
    let heredan: BTreeSet<usize> = lineas
        .iter()
        .zip(&leidas)
        .filter_map(|(l, x)| x.as_ref().filter(|x| x.ip == de && marcada(l)).and_then(gemela))
        .collect();
    let mut cambiadas: Vec<String> = Vec::new();
    let mut fuera = String::with_capacity(actual.len());
    for (i, (l, x)) in lineas.iter().zip(&leidas).enumerate() {
        match x {
            Some(x) if x.ip == de => {
                if gemela(x).is_none() {
                    fuera.push_str(&linea_con_otra_direccion(l, de, a));
                }
                if !cambiadas.contains(&x.base) {
                    cambiadas.push(x.base.clone());
                }
            }
            Some(_) if heredan.contains(&i) && !marcada(l) => {
                let mut c: Vec<String> = l.split(':').map(String::from).collect();
                c[2].push('*');
                fuera.push_str(&c.join(":"));
            }
            _ => fuera.push_str(l),
        }
    }
    (fuera, cambiadas)
}

/// ¿Hay un PostgreSQL en esa dirección? Se le hace la pregunta con la que empieza cualquier conexión
/// (SSLRequest) y tiene que contestar «S» o «N»: que el puerto esté abierto no basta (en casa, esa
/// misma dirección puede ser otro aparato). No manda usuario ni clave. Si no, dice por qué.
fn probar_postgres(ip: &str, espera: std::time::Duration) -> Result<(), String> {
    use std::io::{ErrorKind, Read};
    use std::net::{IpAddr, SocketAddr, TcpStream};
    let por_que = |e: std::io::Error| match e.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "no contesta a tiempo".to_string(),
        ErrorKind::ConnectionRefused => "rechaza la conexión".to_string(),
        // En macOS 15 o más nuevo, sin el permiso de «Red local» las conexiones a la red propia (y a
        // ZeroTier, que es otra red propia) fallan así, al instante.
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable if cfg!(target_os = "macos") => {
            "sin camino hasta ella; si la red está conectada, falta el permiso: Ajustes del Sistema › Privacidad y seguridad › Red local › Labstream OS".to_string()
        }
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => "sin camino hasta ella".to_string(),
        _ => e.to_string(),
    };
    let ip: IpAddr = ip.parse().map_err(|_| "dirección mal escrita".to_string())?;
    let mut s = TcpStream::connect_timeout(&SocketAddr::new(ip, PUERTO_POSTGRES), espera).map_err(por_que)?;
    let _ = s.set_read_timeout(Some(espera));
    let _ = s.set_write_timeout(Some(espera));
    s.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).map_err(por_que)?;
    let mut r = [0u8; 1];
    s.read_exact(&mut r).map_err(por_que)?;
    if matches!(r[0], b'S' | b'N') {
        Ok(())
    } else {
        Err("contesta algo que no es PostgreSQL".into())
    }
}

fn espera_de(ip: &str) -> std::time::Duration {
    // En la oficina contesta en milisegundos; por ZeroTier, con el túnel recién levantado, tarda más.
    std::time::Duration::from_millis(if ip == SERVIDOR.0 { 1500 } else { 4000 })
}

fn llega(ip: &str) -> bool {
    probar_postgres(ip, espera_de(ip)).is_ok()
}

// ── El diagnóstico (1.40.2, 12-nov-2026) ─────────────────────────────────────────────────────
//
// «No me cargan las bibliotecas en mi Mac»: desde fuera no se veía por qué. Esto lo dice en frases,
// para leerlo o copiarlo desde la pestaña Resolve del OS: dónde está la lista, cuántas bibliotecas
// entran por cada dirección, si Resolve está abierto y si el servidor contesta por la oficina y por
// ZeroTier (y si no, por qué). Nunca una línea de la lista: llevan la clave.

fn diagnostico(modo: Modo, version: &str, abierto: fn() -> bool, probar: &dyn Fn(&str) -> Result<(), String>) -> Vec<String> {
    let sistema = if cfg!(windows) { "Windows" } else if cfg!(target_os = "macos") { "Mac" } else { "otro sistema" };
    let mut d = vec![format!("App {version} · {sistema} · modo: {}", modo.etiqueta())];
    let Some((fijas, _)) = donde_buscar() else {
        d.push("En este sistema la app no sabe dónde guarda Resolve su lista.".into());
        return d;
    };
    match ruta_lista().filter(|p| p.is_file()) {
        Some(p) => {
            d.push(format!("Lista de Resolve: {}", legible(&p)));
            match std::fs::read_to_string(&p) {
                Ok(texto) => {
                    let lineas: Vec<Linea> = texto.lines().filter_map(leer_linea).collect();
                    let de = |ip: &str| lineas.iter().filter(|l| l.ip == ip).count();
                    let (of, zt) = (de(SERVIDOR.0), de(SERVIDOR.1));
                    d.push(format!("Bibliotecas de red: {} ({of} por la oficina, {zt} por ZeroTier, {} de otra dirección)", lineas.len(), lineas.len() - of - zt));
                }
                Err(e) => d.push(format!("No pude leerla: {e}")),
            }
        }
        None => {
            d.push("Lista de Resolve: no la encontré (dblist.conf). Busqué en:".into());
            d.extend(fijas.iter().map(|f| format!("  {}", legible(f))));
        }
    }
    d.push(format!("Resolve: {}", if abierto() { "abierto (la lista solo se cambia con Resolve cerrado)" } else { "cerrado" }));
    for (nombre, ip) in [("Oficina", SERVIDOR.0), ("ZeroTier", SERVIDOR.1)] {
        d.push(match probar(ip) {
            Ok(()) => format!("{nombre} ({ip}): el servidor responde"),
            Err(e) => format!("{nombre} ({ip}): no responde ({e})"),
        });
    }
    d
}

#[tauri::command]
pub async fn resolve_diagnostico<R: Runtime>(app: AppHandle<R>) -> Vec<String> {
    let m = modo(&app);
    let version = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || diagnostico(m, &version, resolve_abierto, &|ip| probar_postgres(ip, espera_de(ip))))
        .await
        .unwrap_or_else(|_| vec!["No pude hacer el diagnóstico.".into()])
}

#[derive(Serialize, Debug, PartialEq)]
pub struct RedLista {
    /// El modo de este equipo (1.40). Con `apagado` no se mira nada y todo va a cero.
    pub(crate) modo: Modo,
    /// Por dónde entra ahora: la red que se eligió a mano, o en Automático por dónde llega este equipo
    /// al servidor. None: no llega por ninguna, apagado, o la lista no tiene nada de este servidor.
    pub(crate) red: Option<Red>,
    /// Elegida a mano: si esa dirección contesta ahora. En Automático, None (ya se eligió la que contesta).
    pub(crate) responde: Option<bool>,
    /// Cuántas bibliotecas entran por cada dirección, tal como quedó la lista.
    pub(crate) oficina: u32,
    pub(crate) zerotier: u32,
    /// Las que cambiaron de dirección en esta llamada.
    pub(crate) cambiadas: Vec<String>,
    /// Las que están con la otra dirección y no se cambiaron (Resolve abierto, o solo se preguntó).
    pub(crate) pendientes: u32,
    pub(crate) abierto: bool,
    copia: Option<String>,
}

fn red_lista(ruta: Option<PathBuf>, aplicar: bool, modo: Modo, abierto: fn() -> bool, llega: &dyn Fn(&str) -> bool) -> Result<RedLista, String> {
    let (oficina, zerotier) = SERVIDOR;
    let cuenta = |texto: &str, ip: &str| texto.lines().filter_map(leer_linea).filter(|l| l.ip == ip).count() as u32;
    let vacia = RedLista { modo, red: None, responde: None, oficina: 0, zerotier: 0, cambiadas: vec![], pendientes: 0, abierto: false, copia: None };
    // Apagado: ni se lee la lista ni se pregunta a la red.
    if modo == Modo::Apagado {
        return Ok(vacia);
    }
    let Some(ruta) = ruta else { return Ok(vacia) };
    let actual = match std::fs::read_to_string(&ruta) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vacia),
        Err(e) => return Err(format!("No pude leer la lista de Resolve: {e}")),
    };
    let (of, zt) = (cuenta(&actual, oficina), cuenta(&actual, zerotier));
    // Sin ninguna línea de este servidor no hay nada que mirar: ni se toca la red.
    if of + zt == 0 {
        return Ok(vacia);
    }
    let (red, responde) = match modo {
        // A mano: esa dirección aunque la otra conteste. Se pregunta solo para poder decir si contesta.
        Modo::Oficina => (Some(Red::Oficina), Some(llega(oficina))),
        Modo::Zerotier => (Some(Red::Zerotier), Some(llega(zerotier))),
        _ => {
            let en_oficina = llega(oficina);
            (red_de(en_oficina, !en_oficina && llega(zerotier)), None)
        }
    };
    let base = RedLista { modo, red, responde, oficina: of, zerotier: zt, cambiadas: vec![], pendientes: 0, abierto: false, copia: None };
    let Some(red) = red else { return Ok(RedLista { abierto: abierto(), ..base }) };
    let (de, a, pendientes) = match red {
        Red::Oficina => (zerotier, oficina, zt),
        Red::Zerotier => (oficina, zerotier, of),
    };
    if pendientes == 0 {
        return Ok(RedLista { abierto: abierto(), ..base });
    }
    if !aplicar || abierto() {
        return Ok(RedLista { pendientes, abierto: abierto(), ..base });
    }
    let (texto, cambiadas) = cambiar_direccion(&actual, de, a);
    let copia = copiar_lista(&ruta)?;
    // Última comprobación justo antes de escribir: si Resolve se abrió mientras tanto, nada.
    if abierto() {
        return Ok(RedLista { pendientes, abierto: true, ..base });
    }
    // Aparte y de un golpe: si esto se corta a medias, la lista sigue entera.
    let temporal = ruta.with_extension("labstream-nueva");
    std::fs::write(&temporal, &texto).map_err(|e| format!("No pude escribir en la lista de Resolve: {e}"))?;
    std::fs::rename(&temporal, &ruta).map_err(|e| format!("No pude escribir en la lista de Resolve: {e}"))?;
    Ok(RedLista {
        oficina: cuenta(&texto, oficina),
        zerotier: cuenta(&texto, zerotier),
        cambiadas,
        copia: copia.file_name().and_then(|n| n.to_str()).map(String::from),
        ..base
    })
}

/// Aplica el modo guardado ahora mismo (el menú ⋮, al elegir). Bloquea: llamarlo fuera del hilo de la ventana.
pub(crate) fn aplicar_modo<R: Runtime>(app: &AppHandle<R>) -> Result<RedLista, String> {
    let m = modo(app);
    let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
    red_lista(ruta_lista(), true, m, resolve_abierto, &llega)
}

/// El aviso del menú ⋮ después de elegir: qué quedó y qué falta.
pub(crate) fn frase_de_modo(r: &RedLista) -> String {
    if r.modo == Modo::Apagado {
        return "Resolve apagado en este equipo: la app ya no toca la lista de bibliotecas.".into();
    }
    let total = r.oficina + r.zerotier;
    let Some(red) = r.red else {
        return if total == 0 {
            "Listo. Este equipo todavía no tiene bibliotecas del servidor en Resolve: las nuevas entrarán así.".into()
        } else {
            format!(
                "Automático: ahora este equipo no llega al servidor ni por la oficina ni por ZeroTier. No cambié nada.{}",
                if cfg!(target_os = "macos") { " Si ZeroTier está conectado, revisa Ajustes del Sistema › Privacidad y seguridad › Red local › Labstream OS." } else { "" }
            )
        };
    };
    let donde = if red == Red::Oficina { "la red de la oficina" } else { "ZeroTier" };
    let mut s = if !r.cambiadas.is_empty() {
        let n = r.cambiadas.len();
        format!("Resolve entra por {donde}: {} a esa dirección.", if n == 1 { "1 biblioteca pasó".to_string() } else { format!("{n} bibliotecas pasaron") })
    } else if r.pendientes > 0 && r.abierto {
        format!("Resolve entrará por {donde}: ciérralo y las bibliotecas se cambian.")
    } else {
        format!("Resolve entra por {donde}. Todo en orden.")
    };
    if r.responde == Some(false) {
        s.push_str(&format!(" Ojo: ahora {donde} no responde."));
    }
    s
}

/// `aplicar: false` solo dice por dónde entra y cómo está la lista (para el bloque del OS). Sigue el
/// modo guardado de este equipo.
#[tauri::command]
pub async fn resolve_lista_red<R: Runtime>(app: AppHandle<R>, aplicar: bool) -> Result<RedLista, String> {
    let m = modo(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        red_lista(ruta_lista(), aplicar, m, resolve_abierto, &llega)
    })
    .await
    .map_err(|e| format!("No pude mirar por dónde entra Resolve: {e}"))?
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn l(base: &str, ip: &str, rol: &str, clave: &str) -> Linea {
        Linea { ip: ip.into(), rol: rol.into(), clave: clave.into(), base: base.into() }
    }

    #[test]
    fn lee_y_escribe_la_misma_linea_que_el_panel() {
        let s = escribir_linea("Camilo_Ortega", "192.168.0.22", "camilo", "s3creta");
        assert_eq!(s, "Camilo_Ortega192.168.0.22:192.168.0.22:camilo:s3creta:Camilo_Ortega:QPSQL");
        assert_eq!(leer_linea(&(s + "\r\n")), Some(l("Camilo_Ortega", "192.168.0.22", "camilo", "s3creta")));
    }

    #[test]
    fn la_marca_de_la_ultima_biblioteca_es_el_mismo_usuario() {
        let ls: Vec<Linea> = ["A1:1:camilo:k:A:QPSQL", "B1:1:camilo*:k:B:QPSQL"].iter().filter_map(|x| leer_linea(x)).collect();
        assert_eq!(ls[1], l("B", "1", "camilo", "k"));
        assert_eq!(cuentas_de(&ls), vec![Cuenta { rol: "camilo".into(), ip: "1".into(), lineas: 2 }]);
        assert_eq!(leer_linea("X1:1:*:k:X:QPSQL"), None);
        // Un equipo con UNA sola biblioteca (la marcada) también recibe las nuevas, sin la marca.
        let solo: Vec<Linea> = leer_linea("Unica1:1:ana*:k9:Unica:QPSQL").into_iter().collect();
        assert_eq!(clave_de(&solo, "ana", "1").as_deref(), Some("k9"));
        assert_eq!(escribir_linea("Nueva", "1", &solo[0].rol, "k9"), "Nueva1:1:ana:k9:Nueva:QPSQL");
    }

    #[test]
    fn la_base_local_y_lo_roto_no_cuentan() {
        assert_eq!(leer_linea("Local Database:C:\\ProgramData\\Blackmagic Design:x:y:z:DISK"), None);
        assert_eq!(leer_linea(""), None);
        assert_eq!(leer_linea("a:b:c:QPSQL"), None);
        assert_eq!(leer_linea("x:192.168.0.22::k:x:QPSQL"), None);
    }

    #[test]
    fn cuentas_sin_claves_y_en_orden() {
        let ls = [l("A", "192.168.0.22", "camilo", "k"), l("B", "10.147.17.5", "camilo", "k"), l("C", "192.168.0.22", "camilo", "k")];
        assert_eq!(
            cuentas_de(&ls),
            vec![Cuenta { rol: "camilo".into(), ip: "192.168.0.22".into(), lineas: 2 }, Cuenta { rol: "camilo".into(), ip: "10.147.17.5".into(), lineas: 1 }]
        );
    }

    #[test]
    fn la_clave_de_la_mayoria_y_a_igualdad_la_mas_reciente() {
        let ls = [l("A", "ip", "u", "vieja"), l("B", "ip", "u", "nueva"), l("C", "ip", "u", "nueva"), l("D", "otra", "u", "x")];
        assert_eq!(clave_de(&ls, "u", "ip").as_deref(), Some("nueva"));
        let empate = [l("A", "ip", "u", "vieja"), l("B", "ip", "u", "nueva")];
        assert_eq!(clave_de(&empate, "u", "ip").as_deref(), Some("nueva"));
        assert_eq!(clave_de(&ls, "nadie", "ip"), None);
    }

    #[test]
    fn nombres_que_no_rompen_la_lista() {
        assert!(nombre_valido("Dra_Danney_Gomez"));
        assert!(nombre_valido("Pruebas 2026.v2"));
        assert!(!nombre_valido("a:b"));
        assert!(!nombre_valido("a\nb"));
        assert!(!nombre_valido("_Plantilla"));
        assert!(!nombre_valido(""));
        assert!(!nombre_valido(&"x".repeat(61)));
    }

    #[test]
    fn respeta_el_salto_del_archivo() {
        let n = vec!["L1".to_string(), "L2".to_string()];
        assert_eq!(texto_a_anadir("a\r\nb\r\n", &n), "L1\r\nL2\r\n");
        assert_eq!(texto_a_anadir("a\nb", &n), "\nL1\nL2\n");
        assert_eq!(texto_a_anadir("a\r\nb", &n), "\r\nL1\r\nL2\r\n");
    }

    fn carpeta(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ls-resolve-lista-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn cerrado() -> bool {
        false
    }
    fn abierto() -> bool {
        true
    }
    const LISTA: &str = "Local Database:C/x:y:z:w:DISK\r\nCamilo_Ortega192.168.0.22:192.168.0.22:camilo:k1:Camilo_Ortega:QPSQL\r\n";

    fn r(de: &str, a: &str) -> Renombre {
        Renombre { de: de.into(), a: a.into() }
    }

    #[test]
    fn cambia_el_nombre_y_conserva_direccion_usuario_clave_y_marca() {
        let antes = "Viejo192.168.0.22:192.168.0.22:camilo*:k1:Viejo:QPSQL\r\nOtra1:1:camilo:k1:Otra:QPSQL\r\n";
        let (texto, hechos) = cambiar_nombres(antes, &[r("Viejo", "Nuevo")]);
        assert_eq!(texto, "Nuevo192.168.0.22:192.168.0.22:camilo*:k1:Nuevo:QPSQL\r\nOtra1:1:camilo:k1:Otra:QPSQL\r\n");
        assert_eq!(hechos, vec![r("Viejo", "Nuevo")]);
    }

    #[test]
    fn un_primer_campo_que_no_empieza_por_la_base_se_respeta() {
        let (texto, _) = cambiar_nombres("Mi biblioteca:1:ana:k:Viejo:QPSQL", &[r("Viejo", "Nuevo")]);
        assert_eq!(texto, "Mi biblioteca:1:ana:k:Nuevo:QPSQL");
    }

    #[test]
    fn la_base_local_y_lo_que_no_esta_no_se_tocan() {
        let (texto, hechos) = cambiar_nombres(LISTA, &[r("Otra", "Nueva"), r("Local Database", "Otra")]);
        assert_eq!(texto, LISTA);
        assert!(hechos.is_empty());
    }

    #[test]
    fn va_en_orden_y_si_la_nueva_ya_estaba_la_vieja_sobra() {
        let (texto, hechos) = cambiar_nombres("A1:1:u:k:A:QPSQL\n", &[r("A", "B"), r("B", "C")]);
        assert_eq!(texto, "C1:1:u:k:C:QPSQL\n");
        assert_eq!(hechos.len(), 2);
        let (texto, _) = cambiar_nombres("A1:1:u*:k:A:QPSQL\nB1:1:u:k:B:QPSQL\n", &[r("A", "B")]);
        assert_eq!(texto, "B1:1:u:k:B:QPSQL\n");
    }

    #[test]
    fn un_nombre_que_rompe_la_lista_no_pasa() {
        let (texto, hechos) = cambiar_nombres(LISTA, &[r("Camilo_Ortega", "a:b"), r("Camilo_Ortega", "postgres")]);
        assert_eq!(texto, LISTA);
        assert!(hechos.is_empty());
    }

    #[test]
    fn renombra_con_copia_y_al_dia_lo_visto() {
        let d = carpeta("renombrar");
        let (lista, vistas) = (d.join("dblist.conf"), d.join("vistas.json"));
        std::fs::write(&lista, LISTA).unwrap();
        mirar(Some(lista.clone()), Some(&vistas), cerrado);
        let h = renombrar(Some(lista.clone()), Some(&vistas), &[r("Camilo_Ortega", "Camilo_O")], cerrado).unwrap();
        assert_eq!(h.renombradas, vec!["Camilo_O".to_string()]);
        let texto = std::fs::read_to_string(&lista).unwrap();
        assert!(texto.contains("Camilo_O192.168.0.22:192.168.0.22:camilo:k1:Camilo_O:QPSQL\r\n"), "{texto}");
        assert!(texto.starts_with("Local Database:C/x:y:z:w:DISK\r\n"));
        assert_eq!(std::fs::read_to_string(d.join(h.copia.unwrap())).unwrap(), LISTA);
        let v = std::fs::read_to_string(&vistas).unwrap();
        assert!(v.contains("Camilo_O") && !v.contains("Camilo_Ortega"), "{v}");
        assert!(!d.join("dblist.labstream-nueva").exists());
    }

    #[test]
    fn con_resolve_abierto_no_renombra_ni_sin_nada_que_hacer_escribe() {
        let d = carpeta("renombrar-abierto");
        let lista = d.join("dblist.conf");
        std::fs::write(&lista, LISTA).unwrap();
        assert!(renombrar(Some(lista.clone()), None, &[r("Camilo_Ortega", "Camilo_O")], abierto).is_err());
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), LISTA);
        let h = renombrar(Some(lista.clone()), None, &[r("Otra", "Nueva")], cerrado).unwrap();
        assert!(h.renombradas.is_empty() && h.copia.is_none());
    }

    #[test]
    fn mira_sin_claves_y_anota_lo_visto() {
        let d = carpeta("mirar");
        let (lista, vistas) = (d.join("dblist.conf"), d.join("vistas.json"));
        std::fs::write(&lista, LISTA).unwrap();
        let e = mirar(Some(lista.clone()), Some(&vistas), cerrado);
        let json = serde_json::to_string(&e).unwrap();
        assert!(e.disponible && !e.abierto);
        assert_eq!(e.nombres, vec!["Camilo_Ortega".to_string()]);
        assert!(!json.contains("k1"), "la clave no sale nunca: {json}");
        assert!(std::fs::read_to_string(&vistas).unwrap().contains("Camilo_Ortega"));
        assert!(!mirar(Some(d.join("no-esta.conf")), None, cerrado).disponible);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn anade_al_final_con_copia_y_sin_repetir() {
        let d = carpeta("agregar");
        let (lista, vistas) = (d.join("dblist.conf"), d.join("vistas.json"));
        std::fs::write(&lista, LISTA).unwrap();
        let paso = |b: &[&str]| Paso { rol: "camilo".into(), ip: "192.168.0.22".into(), bibliotecas: b.iter().map(|s| s.to_string()).collect(), forzar: false };

        let r = agregar(Some(lista.clone()), Some(&vistas), paso(&["Camilo_Ortega", "Nueva", "a:b", "Nueva"]), cerrado).unwrap();
        assert_eq!(r.agregadas, vec!["Nueva".to_string()]);
        let despues = std::fs::read_to_string(&lista).unwrap();
        assert_eq!(despues, format!("{LISTA}Nueva192.168.0.22:192.168.0.22:camilo:k1:Nueva:QPSQL\r\n"));
        let copia = d.join(r.copia.unwrap());
        assert_eq!(std::fs::read_to_string(copia).unwrap(), LISTA);

        // Ya está (y ya vista): nada. Si alguien la quita a mano, tampoco vuelve.
        assert!(agregar(Some(lista.clone()), Some(&vistas), paso(&["Nueva"]), cerrado).unwrap().agregadas.is_empty());
        std::fs::write(&lista, LISTA).unwrap();
        assert!(agregar(Some(lista.clone()), Some(&vistas), paso(&["Nueva"]), cerrado).unwrap().agregadas.is_empty());
        // Salvo que se pida (1.40.3, «Volver a ponerlas»): entonces sí vuelve, una sola vez.
        let pedida = Paso { forzar: true, ..paso(&["Nueva", "Camilo_Ortega"]) };
        assert_eq!(agregar(Some(lista.clone()), Some(&vistas), pedida, cerrado).unwrap().agregadas, vec!["Nueva".to_string()]);
        assert!(std::fs::read_to_string(&lista).unwrap().ends_with("Nueva192.168.0.22:192.168.0.22:camilo:k1:Nueva:QPSQL\r\n"));
        let otra_vez = Paso { forzar: true, ..paso(&["Nueva"]) };
        assert!(agregar(Some(lista.clone()), Some(&vistas), otra_vez, cerrado).unwrap().agregadas.is_empty());
        std::fs::write(&lista, LISTA).unwrap();
        // La página manda `forzar` solo al pedirlo; sin el campo es una vuelta normal.
        assert!(!serde_json::from_str::<Paso>(r#"{"rol":"camilo","ip":"1","bibliotecas":[]}"#).unwrap().forzar);
        assert!(serde_json::from_str::<Paso>(r#"{"rol":"camilo","ip":"1","bibliotecas":[],"forzar":true}"#).unwrap().forzar);

        // Con Resolve abierto, o con un usuario o dirección que no están en la lista: error y sin tocar.
        assert!(agregar(Some(lista.clone()), Some(&vistas), paso(&["Otra"]), abierto).is_err());
        let ajeno = Paso { rol: "camilo".into(), ip: "6.6.6.6".into(), bibliotecas: vec!["Otra".into()], forzar: false };
        assert!(agregar(Some(lista.clone()), Some(&vistas), ajeno, cerrado).is_err());
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), LISTA);
        let _ = std::fs::remove_dir_all(&d);
    }

    // ── Dónde está la lista (1.40.2) ──

    #[test]
    fn encuentra_la_lista_donde_resolve_la_tenga_y_prefiere_la_que_tiene_bibliotecas() {
        let d = carpeta("donde");
        let prefs = d.join("Library/Preferences/Blackmagic Design/DaVinci Resolve");
        let soporte = d.join("Library/Application Support/Blackmagic Design/DaVinci Resolve/Preferences");
        std::fs::create_dir_all(&prefs).unwrap();
        std::fs::create_dir_all(&soporte).unwrap();
        let fijas = vec![prefs.join("dblist.conf"), soporte.join("dblist.conf")];

        // Ninguna existe.
        assert_eq!(elegir_lista(&fijas), None);
        // Solo la del sitio viejo, y sin bibliotecas de red: es la que hay.
        std::fs::write(&fijas[1], "Local Database:C/x:y:z:w:DISK\n").unwrap();
        assert_eq!(elegir_lista(&fijas), Some(fijas[1].clone()));
        // La de las preferencias tiene bibliotecas de red: gana aunque la otra también exista.
        std::fs::write(&fijas[0], format!("A{OF}:{OF}:camilo:k1:A:QPSQL\n")).unwrap();
        assert_eq!(elegir_lista(&fijas), Some(fijas[0].clone()));
        // Y al revés: gana la que tiene bibliotecas, no la primera.
        std::fs::write(&fijas[0], "Local Database:C/x:y:z:w:DISK\n").unwrap();
        std::fs::write(&fijas[1], format!("A{OF}:{OF}:camilo:k1:A:QPSQL\n")).unwrap();
        assert_eq!(elegir_lista(&fijas), Some(fijas[1].clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn busca_la_lista_bajo_las_carpetas_de_blackmagic_sin_entrar_en_los_proyectos() {
        let d = carpeta("buscar");
        let rara = d.join("DaVinci Resolve/Otra/Sitio");
        let proyectos = d.join("DaVinci Resolve/Resolve Project Library/Resolve Projects");
        let honda = d.join("a/b/c/d/e/f/g");
        for c in [&rara, &proyectos, &honda] {
            std::fs::create_dir_all(c).unwrap();
            std::fs::write(c.join("dblist.conf"), "x").unwrap();
        }
        assert_eq!(buscar_lista(&d, FONDO_DE_BUSQUEDA), vec![rara.join("dblist.conf")]);
        assert_eq!(buscar_lista(&d.join("no-existe"), FONDO_DE_BUSQUEDA), Vec::<PathBuf>::new());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn el_diagnostico_dice_el_modo_resolve_y_por_que_no_responde_sin_ninguna_clave() {
        let solo_zt = |ip: &str| if ip == ZT { Ok(()) } else { Err("no contesta a tiempo".to_string()) };
        let d = diagnostico(Modo::Zerotier, "1.40.2", abierto, &solo_zt);
        assert!(d[0].starts_with("App 1.40.2 · "));
        assert!(d[0].ends_with("modo: ZeroTier (desde fuera)"));
        assert!(d.iter().any(|l| l.starts_with("Resolve: abierto")));
        assert!(d.contains(&format!("Oficina ({OF}): no responde (no contesta a tiempo)")));
        assert!(d.contains(&format!("ZeroTier ({ZT}): el servidor responde")));
        assert!(d.iter().all(|l| !l.contains("QPSQL")));
    }

    // ── La oficina o ZeroTier ──

    fn red_auto(ruta: Option<PathBuf>, aplicar: bool, abierto: fn() -> bool, llega: &dyn Fn(&str) -> bool) -> Result<RedLista, String> {
        red_lista(ruta, aplicar, Modo::Auto, abierto, llega)
    }

    #[test]
    fn el_modo_viaja_con_su_nombre_y_por_defecto_es_automatico() {
        for m in Modo::TODOS {
            assert_eq!(Modo::de_id(m.id()), Some(m));
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{}\"", m.id()));
        }
        assert_eq!(Modo::de_id("otro"), None);
        assert_eq!(serde_json::from_str::<ConfigModo>("{}").unwrap().modo, Modo::Auto);
    }

    #[test]
    fn a_mano_manda_la_red_elegida_y_apagado_no_toca_nada() {
        let d = carpeta("modo");
        let lista = d.join("dblist.conf");
        let de_oficina = format!("A{OF}:{OF}:camilo*:k1:A:QPSQL\r\nB{OF}:{OF}:camilo:k1:B:QPSQL\r\n");
        std::fs::write(&lista, &de_oficina).unwrap();
        let en_oficina = |ip: &str| ip == OF || ip == ZT;

        // Apagado: ni la lista ni la red.
        let nunca = |_: &str| -> bool { panic!("apagado: no se pregunta a la red") };
        let r = red_lista(Some(lista.clone()), true, Modo::Apagado, cerrado, &nunca).unwrap();
        assert_eq!((r.modo, r.red, r.oficina, r.zerotier, r.cambiadas.len()), (Modo::Apagado, None, 0, 0, 0));
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), de_oficina);
        assert!(frase_de_modo(&r).contains("apagado"));

        // ZeroTier a mano estando en la oficina (las dos contestan): pasan a ZeroTier igual.
        let r = red_lista(Some(lista.clone()), true, Modo::Zerotier, cerrado, &en_oficina).unwrap();
        assert_eq!((r.red, r.responde, r.oficina, r.zerotier), (Some(Red::Zerotier), Some(true), 0, 2));
        assert!(frase_de_modo(&r).starts_with("Resolve entra por ZeroTier: 2 bibliotecas pasaron"));

        // Local a mano desde casa (solo contesta ZeroTier): vuelven a la oficina y avisa que no responde.
        let en_casa = |ip: &str| ip == ZT;
        let r = red_lista(Some(lista.clone()), true, Modo::Oficina, cerrado, &en_casa).unwrap();
        assert_eq!((r.red, r.responde, r.oficina, r.zerotier), (Some(Red::Oficina), Some(false), 2, 0));
        assert!(frase_de_modo(&r).ends_with("Ojo: ahora la red de la oficina no responde."));
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), de_oficina);

        // Con Resolve abierto: queda pendiente y lo dice.
        let r = red_lista(Some(lista.clone()), true, Modo::Zerotier, abierto, &en_oficina).unwrap();
        assert_eq!((r.pendientes, r.abierto), (2, true));
        assert_eq!(frase_de_modo(&r), "Resolve entrará por ZeroTier: ciérralo y las bibliotecas se cambian.");
        let _ = std::fs::remove_dir_all(&d);
    }

    const OF: &str = "192.168.0.22";
    const ZT: &str = "10.61.5.96";

    #[test]
    fn la_oficina_gana_aunque_zerotier_tambien_responda() {
        assert_eq!(red_de(true, true), Some(Red::Oficina));
        assert_eq!(red_de(true, false), Some(Red::Oficina));
        assert_eq!(red_de(false, true), Some(Red::Zerotier));
        assert_eq!(red_de(false, false), None);
    }

    #[test]
    fn cambia_la_direccion_y_conserva_usuario_clave_marca_y_lo_ajeno() {
        let antes = format!("Local Database:C/x:y:z:w:DISK\r\nA{OF}:{OF}:camilo*:k1:A:QPSQL\r\nB{OF}:{OF}:jona:k2:B:QPSQL\r\nC6.6.6.6:6.6.6.6:camilo:k1:C:QPSQL\r\n");
        let (texto, cambiadas) = cambiar_direccion(&antes, OF, ZT);
        assert_eq!(texto, format!("Local Database:C/x:y:z:w:DISK\r\nA{ZT}:{ZT}:camilo*:k1:A:QPSQL\r\nB{ZT}:{ZT}:jona:k2:B:QPSQL\r\nC6.6.6.6:6.6.6.6:camilo:k1:C:QPSQL\r\n"));
        assert_eq!(cambiadas, vec!["A".to_string(), "B".to_string()]);
        // Y de vuelta queda como estaba, letra por letra.
        assert_eq!(cambiar_direccion(&texto, ZT, OF).0, antes);
        // Nada que cambiar: idéntico.
        assert_eq!(cambiar_direccion(&antes, ZT, OF), (antes.clone(), vec![]));
    }

    #[test]
    fn si_ya_estaba_con_la_otra_direccion_no_sale_dos_veces_y_la_marca_se_queda() {
        let antes = format!("A{OF}:{OF}:camilo*:k1:A:QPSQL\nA{ZT}:{ZT}:camilo:k1:A:QPSQL\nA{ZT}:{ZT}:jona:k2:A:QPSQL\nB{OF}:{OF}:camilo:k1:B:QPSQL\n");
        let (texto, cambiadas) = cambiar_direccion(&antes, OF, ZT);
        assert_eq!(texto, format!("A{ZT}:{ZT}:camilo*:k1:A:QPSQL\nA{ZT}:{ZT}:jona:k2:A:QPSQL\nB{ZT}:{ZT}:camilo:k1:B:QPSQL\n"));
        assert_eq!(cambiadas, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn la_lista_sigue_a_la_red_con_resolve_cerrado_y_con_copia() {
        let d = carpeta("red");
        let lista = d.join("dblist.conf");
        let de_oficina = format!("Local Database:C/x:y:z:w:DISK\r\nA{OF}:{OF}:camilo*:k1:A:QPSQL\r\nB{OF}:{OF}:camilo:k1:B:QPSQL\r\n");
        std::fs::write(&lista, &de_oficina).unwrap();
        let en_oficina = |ip: &str| ip == OF || ip == ZT; // en la oficina responden las dos
        let en_casa = |ip: &str| ip == ZT;
        let sin_red = |_: &str| false;

        // En la oficina y ya con la dirección de oficina: nada, y sin copia.
        let r = red_auto(Some(lista.clone()), true, cerrado, &en_oficina).unwrap();
        assert_eq!((r.red, r.oficina, r.zerotier, r.pendientes, r.copia.is_some()), (Some(Red::Oficina), 2, 0, 0, false));

        // En casa: solo preguntar no cambia nada; con Resolve abierto, tampoco.
        let r = red_auto(Some(lista.clone()), false, cerrado, &en_casa).unwrap();
        assert_eq!((r.red, r.pendientes, r.cambiadas.len()), (Some(Red::Zerotier), 2, 0));
        let r = red_auto(Some(lista.clone()), true, abierto, &en_casa).unwrap();
        assert_eq!((r.pendientes, r.abierto), (2, true));
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), de_oficina);

        // En casa con Resolve cerrado: pasan a ZeroTier, con copia de cómo estaba.
        let r = red_auto(Some(lista.clone()), true, cerrado, &en_casa).unwrap();
        assert_eq!((r.red, r.oficina, r.zerotier, r.pendientes), (Some(Red::Zerotier), 0, 2, 0));
        assert_eq!(r.cambiadas, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(std::fs::read_to_string(d.join(r.copia.unwrap())).unwrap(), de_oficina);
        assert!(std::fs::read_to_string(&lista).unwrap().contains(&format!("A{ZT}:{ZT}:camilo*:k1:A:QPSQL\r\n")));

        // Sin red no se toca; de vuelta en la oficina, vuelve a como estaba.
        let r = red_auto(Some(lista.clone()), true, cerrado, &sin_red).unwrap();
        assert_eq!((r.red, r.zerotier, r.cambiadas.len()), (None, 2, 0));
        let r = red_auto(Some(lista.clone()), true, cerrado, &en_oficina).unwrap();
        assert_eq!((r.red, r.oficina, r.zerotier), (Some(Red::Oficina), 2, 0));
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), de_oficina);

        // Sin lista, o con una lista de otro servidor: ni se pregunta a la red.
        let nunca = |_: &str| -> bool { panic!("no hay nada de este servidor: no se toca la red") };
        assert_eq!(red_auto(Some(d.join("no-esta.conf")), true, cerrado, &nunca).unwrap().red, None);
        std::fs::write(&lista, "C6.6.6.6:6.6.6.6:camilo:k1:C:QPSQL\n").unwrap();
        assert_eq!(red_auto(Some(lista.clone()), true, cerrado, &nunca).unwrap().red, None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Con una COPIA de la lista de este equipo: como si se fuera a casa y volviera. La de verdad no se
    /// toca, y no se imprime ninguna línea (llevan la clave).
    #[test]
    #[ignore]
    fn ida_y_vuelta_con_una_copia_de_la_lista_de_este_equipo() {
        let real = std::fs::read_to_string(ruta_lista().expect("ruta")).expect("este equipo no tiene lista de Resolve");
        let d = carpeta("copia-real");
        let lista = d.join("dblist.conf");
        std::fs::write(&lista, &real).unwrap();
        let en_casa = |ip: &str| ip == ZT;
        let en_oficina = |ip: &str| ip == OF || ip == ZT;
        let antes = red_auto(Some(lista.clone()), false, cerrado, &en_oficina).unwrap();
        let ida = red_auto(Some(lista.clone()), true, cerrado, &en_casa).unwrap();
        println!("en la oficina: {} por oficina, {} por ZeroTier", antes.oficina, antes.zerotier);
        println!("en casa: {} cambiaron; quedan {} por oficina, {} por ZeroTier", ida.cambiadas.len(), ida.oficina, ida.zerotier);
        assert_eq!((ida.oficina, ida.zerotier), (0, antes.oficina + antes.zerotier));
        let lineas = |t: &str| t.lines().count();
        assert_eq!(lineas(&std::fs::read_to_string(&lista).unwrap()), lineas(&real), "ni una línea de más ni de menos");
        let vuelta = red_auto(Some(lista.clone()), true, cerrado, &en_oficina).unwrap();
        println!("de vuelta: {} cambiaron; quedan {} por oficina", vuelta.cambiadas.len(), vuelta.oficina);
        if antes.zerotier == 0 {
            assert!(std::fs::read_to_string(&lista).unwrap() == real, "de vuelta en la oficina, la lista es idéntica a la de verdad");
            println!("la copia quedó idéntica a la lista de verdad");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// En un equipo de verdad: por dónde llega al servidor y cuántas bibliotecas cambiaría. No escribe.
    #[test]
    #[ignore]
    fn en_este_equipo_sin_escribir() {
        let t0 = std::time::Instant::now();
        let r = red_auto(ruta_lista(), false, resolve_abierto, &llega).unwrap();
        println!("{r:?} en {:?}", t0.elapsed());
        println!("oficina responde: {}, zerotier responde: {}, otra dirección: {}", llega(OF), llega(ZT), llega("192.168.0.40"));
    }
}
