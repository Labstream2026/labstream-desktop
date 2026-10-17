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
//! `vistas` (resolve-lista.json en la configuración de la app): lo que ya se vio alguna vez en la
//! lista o se añadió. Si alguien quita una biblioteca a propósito desde Resolve, no se le vuelve a poner.

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
pub(crate) fn leer_linea(l: &str) -> Option<Linea> {
    let l = l.trim_end_matches(['\r', '\n']);
    let c: Vec<&str> = l.split(':').collect();
    if c.len() != 6 || c[5] != "QPSQL" || c[1].is_empty() || c[2].is_empty() || c[4].is_empty() {
        return None;
    }
    Some(Linea { ip: c[1].into(), rol: c[2].into(), clave: c[3].into(), base: c[4].into() })
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

// ── Dónde está la lista y si Resolve está abierto ────────────────────────────────────────────

fn ruta_lista() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Blackmagic Design").join("DaVinci Resolve").join("Preferences").join("dblist.conf"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/Blackmagic Design/DaVinci Resolve/Preferences/dblist.conf"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
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

fn sin_lista(motivo: &str) -> Estado {
    Estado { disponible: false, motivo: Some(motivo.into()), abierto: false, cuentas: vec![], nombres: vec![], vistas: vec![] }
}

fn mirar(ruta: Option<PathBuf>, vistas_en: Option<&Path>, abierto: fn() -> bool) -> Estado {
    let Some(ruta) = ruta else { return sin_lista("Este sistema no tiene la lista de Resolve.") };
    let texto = match std::fs::read_to_string(&ruta) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return sin_lista("Resolve no tiene bibliotecas de red en este equipo."),
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
    let vistas = ruta_vistas(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        mirar(ruta_lista(), vistas.as_deref(), resolve_abierto)
    })
    .await
    .unwrap_or_else(|_| sin_lista("No pude mirar la lista de Resolve."))
}

#[derive(Deserialize)]
pub struct Paso {
    rol: String,
    ip: String,
    bibliotecas: Vec<String>,
}

#[derive(Serialize)]
pub struct Agregado {
    agregadas: Vec<String>,
    copia: Option<String>,
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
        if nombre_valido(b) && !ya.contains(b.as_str()) && !v.vistas.contains(b) && !nuevas.contains(b) {
            nuevas.push(b.clone());
        }
    }
    if nuevas.is_empty() {
        return Ok(Agregado { agregadas: vec![], copia: None });
    }

    // Copia de la lista tal como estaba (se guardan las 5 últimas).
    let dir = ruta.parent().ok_or("La lista de Resolve no tiene carpeta.")?;
    let sello = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let copia = dir.join(format!("{PREFIJO_COPIA}{sello}"));
    std::fs::copy(&ruta, &copia).map_err(|e| format!("No pude guardar la copia de la lista: {e}"))?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut copias: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIJO_COPIA))).collect();
        copias.sort();
        let sobran = copias.len().saturating_sub(COPIAS_QUE_SE_GUARDAN);
        for p in copias.into_iter().take(sobran) {
            let _ = std::fs::remove_file(p);
        }
    }

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
    let vistas = ruta_vistas(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _g = CANDADO.lock().unwrap_or_else(|e| e.into_inner());
        agregar(ruta_lista(), vistas.as_deref(), paso, resolve_abierto)
    })
    .await
    .map_err(|e| format!("No pude añadir las bibliotecas: {e}"))?
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
        let paso = |b: &[&str]| Paso { rol: "camilo".into(), ip: "192.168.0.22".into(), bibliotecas: b.iter().map(|s| s.to_string()).collect() };

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

        // Con Resolve abierto, o con un usuario o dirección que no están en la lista: error y sin tocar.
        assert!(agregar(Some(lista.clone()), Some(&vistas), paso(&["Otra"]), abierto).is_err());
        let ajeno = Paso { rol: "camilo".into(), ip: "6.6.6.6".into(), bibliotecas: vec!["Otra".into()] };
        assert!(agregar(Some(lista.clone()), Some(&vistas), ajeno, cerrado).is_err());
        assert_eq!(std::fs::read_to_string(&lista).unwrap(), LISTA);
        let _ = std::fs::remove_dir_all(&d);
    }
}
