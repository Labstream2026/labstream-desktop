//! Los colores de las carpetas del OS en el Explorador, solos (1.36, 10-nov-2026).
//!
//! Las carpetas que el OS asigna a un cliente llevan en el NAS un desktop.ini que las pinta del color
//! del cliente con «OS» (Labstream OS, lib/carpetas-os). Windows 11 solo lo aplica si:
//!   1. el NAS cuenta como red local: por IP (\\192.168.0.22) Windows lo trata como «Internet» y no
//!      personaliza sus carpetas;
//!   2. el icono está en el propio computador, y en %ProgramData%: desde AppData el Explorador no lo
//!      carga (probado en 26H2).
//!
//! Antes eso era un comando que cada persona pegaba en Windows + R desde el aviso del Inicio. Munka:
//! «quiero que sea muy fácil de implementar». Ahora lo hace la app sola al abrir, sin preguntar y sin
//! ventanas: 20 s después de arrancar, y otra vez cada día. Sin red, reintenta a la media hora. Nada de
//! esto conecta unidades ni da acceso a ninguna carpeta, y nada pide administrador.
//!
//! ── Lo que enseñó la auditoría del 10-nov-2026 (todo medido en Windows 11 26H2) ──────────────────
//!
//! · LA ZONA SE PIDE A WINDOWS, NO SE ESCRIBE EN EL REGISTRO. La 1.36.0 (y el comando del OS) creaban a
//!   mano la clave `ZoneMap\Ranges\Labstream <ip>`. Windows lee esas claves por orden alfabético y SE
//!   DETIENE en la primera cuyo nombre tenga 20 letras o más: «Labstream 10.61.5.27» tiene 20, así que
//!   ni esas entradas ni las que venían después contaban (y un computador que ya las tenía bien puestas
//!   con otro nombre las perdía). Ahora se usa la vía oficial, `IInternetSecurityManager::
//!   SetZoneMapping`: Windows pone él el nombre («Range1»…) y avisa a los programas abiertos, así que el
//!   Explorador lo ve en el momento, sin cerrar la sesión. Escrita a mano, la entrada solo la veían los
//!   programas que se abrieran después.
//!   La primera vez tarda (cada petición, unos 10–15 s: Windows avisa a todas las ventanas abiertas);
//!   va en su propio hilo y no frena nada. Con todo puesto, una vuelta dura menos de un milisegundo.
//! · SE COMPRUEBA PREGUNTÁNDOLE A WINDOWS (`MapUrlToZone`), no mirando si la clave existe. Si la IP ya
//!   es de la red local (la puso alguien a mano, o una directiva), no se toca nada. Si después de pedirlo
//!   Windows la sigue viendo como Internet (una directiva del equipo que manda sobre el usuario), se dice
//!   en el diario de salud y el Inicio enseña el aviso.
//! · SE QUITAN LAS CLAVES VIEJAS de nombre largo (`Labstream <ip>`, solo esas y solo de nuestras IP),
//!   que son las que rompían la lista.
//! · SIN PROGRAMAS DE POR MEDIO para el registro ni para abrir el paquete: antes se llamaba a reg.exe y
//!   tar.exe. Un equipo con las herramientas del registro bloqueadas, o un antivirus que mire mal a un
//!   programa que lanza reg.exe, lo frenaban. El paquete (.tar del OS, público: solo dibujos de colores)
//!   se abre aquí, y solo se aceptan archivos `os-<color>.ico` que de verdad sean un icono.
//! · %ProgramData% deja a cualquier usuario crear carpetas y archivos, pero no reescribir los de OTRO
//!   usuario. Si otra persona del mismo computador ya los instaló, no se pueden pisar y no importa: ya
//!   están. Solo falla si un icono falta y no se puede crear.
//!
//! La página del Inicio pregunta `carpetas_os_estado`: en esta app el aviso del comando ya no sale,
//! salvo que aquí haya fallado.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Manager, Runtime};

const ICONOS_URL: &str = "https://os.labstreamsas.com/api/conector-unidades/iconos.tar";
/// Las IP de los NAS en la oficina y por ZeroTier: las mismas que `IPS` de Labstream OS
/// (lib/conector-unidades.ts). Si cambia una allí, cambia aquí.
pub(crate) const IPS: [&str; 4] = ["192.168.0.22", "10.61.5.96", "192.168.0.40", "10.61.5.27"];
/// Un icono que siempre está en el juego (el gris de la paleta): si existe, los iconos están.
const TESTIGO: &str = "os-64748b.ico";
const PRIMERA: Duration = Duration::from_secs(20);
const CADA: Duration = Duration::from_secs(24 * 3600);
const REINTENTO: Duration = Duration::from_secs(30 * 60);
/// El paquete real pesa unos cientos de KB; más que esto no es el paquete.
const MAX_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ICONO: usize = 2 * 1024 * 1024;
/// Windows deja de leer la lista de zonas en la primera clave con un nombre así de largo (medido).
const NOMBRE_QUE_ROMPE: usize = 20;
#[cfg(windows)]
const RANGES: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings\ZoneMap\Ranges";

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Estado {
    /// No es Windows: en Mac las carpetas no se pintan así.
    NoAplica,
    /// Todavía no terminó la primera vuelta.
    Preparando,
    Listo,
    /// La última vuelta no lo dejó funcionando (sin red, una directiva del equipo…): el Inicio avisa.
    Fallo,
}

static ESTADO: Mutex<Estado> = Mutex::new(if cfg!(windows) { Estado::Preparando } else { Estado::NoAplica });

fn poner(e: Estado) {
    if let Ok(mut g) = ESTADO.lock() {
        *g = e;
    }
}

/// Lo que pregunta la página del Inicio.
#[tauri::command]
pub fn carpetas_os_estado() -> Estado {
    ESTADO.lock().map(|g| *g).unwrap_or(Estado::Preparando)
}

// ── Lo puro ──────────────────────────────────────────────────────────────────────────────────

/// El nombre de la clave que creaban la 1.36.0 y el comando del OS del 9-nov: la que hay que quitar.
pub(crate) fn clave_vieja(ip: &str) -> String {
    format!("Labstream {ip}")
}

/// La zona de Windows que vale: 0 este equipo, 1 intranet local, 2 sitios de confianza. 3 es Internet.
pub(crate) fn zona_local(z: u32) -> bool {
    z <= 2
}

/// ¿Toca bajar los iconos? Si falta el testigo, siempre; si no, una vez al día.
pub(crate) fn toca_bajar(falta_testigo: bool, ultima: Option<SystemTime>, ahora: SystemTime) -> bool {
    falta_testigo || ultima.map_or(true, |u| ahora.duration_since(u).map_or(false, |d| d >= CADA))
}

/// `os-<seis hex en minúscula>.ico` y nada más: ni rutas, ni otras extensiones.
pub(crate) fn nombre_de_icono(n: &str) -> bool {
    n.len() == 13 && n.starts_with("os-") && n.ends_with(".ico") && n[3..9].bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// La cabecera de un .ico: reservado 0, tipo 1 (icono) y al menos una imagen.
pub(crate) fn es_ico(d: &[u8]) -> bool {
    d.len() >= 22 && d[0..4] == [0, 0, 1, 0] && u16::from_le_bytes([d[4], d[5]]) >= 1
}

/// Los iconos de un .tar (ustar) como el que arma el OS. Lo que no sea un `os-<color>.ico` se salta;
/// un icono que no es un icono, o un paquete cortado, es un error: no se instala nada a medias.
pub(crate) fn iconos_del_tar(b: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    if b.len() < 512 || &b[257..262] != b"ustar" {
        return Err("lo que llegó no es el paquete de iconos".into());
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 512 <= b.len() {
        let h = &b[i..i + 512];
        if h.iter().all(|&c| c == 0) {
            break;
        }
        let nombre = String::from_utf8_lossy(h[0..100].split(|&c| c == 0).next().unwrap_or(&[])).into_owned();
        let octal: String = h[124..136].iter().take_while(|&&c| c != 0 && c != b' ').map(|&c| c as char).collect();
        let tam = usize::from_str_radix(octal.trim(), 8).map_err(|_| "el paquete de iconos está dañado".to_string())?;
        let ini = i + 512;
        let fin = ini.checked_add(tam).filter(|&f| f <= b.len()).ok_or("el paquete de iconos llegó cortado")?;
        if (h[156] == b'0' || h[156] == 0) && nombre_de_icono(&nombre) {
            let datos = &b[ini..fin];
            if tam > MAX_ICONO || !es_ico(datos) {
                return Err(format!("{nombre} no es un icono"));
            }
            out.push((nombre, datos));
        }
        i = ini + tam.div_ceil(512) * 512;
    }
    if out.is_empty() {
        return Err("el paquete no trae iconos".into());
    }
    Ok(out)
}

pub(crate) fn carpeta_iconos(program_data: &Path) -> PathBuf {
    program_data.join("Labstream").join("carpetas-os")
}

/// Deja los iconos en `dir`. Uno igual no se toca; uno que no se puede reescribir pero ya está (lo
/// instaló otro usuario del computador) se da por bueno. Devuelve cuántos escribió.
pub(crate) fn instalar_iconos(dir: &Path, iconos: &[(String, &[u8])]) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
    let mut escritos = 0;
    for (nombre, datos) in iconos {
        let ruta = dir.join(nombre);
        if std::fs::read(&ruta).map_or(false, |ya| ya == *datos) {
            continue;
        }
        match std::fs::write(&ruta, datos) {
            Ok(()) => escritos += 1,
            Err(_) if ruta.exists() => {}
            Err(e) => return Err(format!("no se pudo guardar {}: {e}", ruta.display())),
        }
    }
    Ok(escritos)
}

// ── Las zonas de Windows ─────────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod zonas {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::Urlmon::{CoInternetCreateSecurityManager, IInternetSecurityManager};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

    const INTRANET: u32 = 1; // URLZONE_INTRANET
    const CREAR: u32 = 0; // SZM_CREATE
    const QUITAR: u32 = 1; // SZM_DELETE

    /// COM abierto en este hilo mientras viva: solo durante la vuelta, no las 24 h que el hilo duerme.
    pub struct Com(bool);
    pub fn abrir_com() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn gestor() -> Result<IInternetSecurityManager, String> {
        let mut m: Option<IInternetSecurityManager> = None;
        unsafe { CoInternetCreateSecurityManager(None, &mut m, 0) }.map_err(|e| format!("Windows no dio el gestor de zonas: {e}"))?;
        m.ok_or_else(|| "Windows no dio el gestor de zonas".to_string())
    }

    /// En qué zona pone Windows las carpetas de esa IP (lo mismo que decide el Explorador).
    pub fn de(ip: &str) -> Result<u32, String> {
        let url = HSTRING::from(format!("file://{ip}/x"));
        let mut z = 0u32;
        unsafe { gestor()?.MapUrlToZone(&url, &mut z, 0) }.map_err(|e| format!("Windows no dijo la zona de {ip}: {e}"))?;
        Ok(z)
    }

    /// Solo las carpetas compartidas (file://) de esa IP, a «Intranet local», para este usuario.
    pub fn a_intranet(ip: &str) -> Result<(), String> {
        let patron = HSTRING::from(format!("file://{ip}"));
        unsafe { gestor()?.SetZoneMapping(INTRANET, &patron, CREAR) }.map_err(|e| format!("Windows no aceptó {ip} como red local: {e}"))
    }

    pub fn quitar(ip: &str) {
        let patron = HSTRING::from(format!("file://{ip}"));
        if let Ok(g) = gestor() {
            let _ = unsafe { g.SetZoneMapping(INTRANET, &patron, QUITAR) };
        }
    }

    /// Que los programas abiertos (el Explorador) vuelvan a leer la lista de zonas. Hace falta después
    /// de quitar claves a mano del registro: eso no avisa a nadie. Windows solo avisa cuando el cambio
    /// se le pide a él, así que se pone y se quita una IP de documentación (192.0.2.x no existe en
    /// ninguna red).
    pub fn refrescar() {
        const DE_PRUEBA: &str = "192.0.2.254";
        if a_intranet(DE_PRUEBA).is_ok() {
            quitar(DE_PRUEBA);
        }
    }
}

/// Quita las claves de nombre largo que dejaron la 1.36.0 y el comando del OS (solo las nuestras: ese
/// nombre exacto y esa misma IP dentro). Devuelve cuántas quitó y, si queda alguna AJENA de nombre
/// largo, su nombre: esa hace que Windows ignore las que vienen después y no es nuestra para borrarla.
#[cfg(windows)]
fn limpiar_claves_viejas(ips: &[&str]) -> (usize, Option<String>) {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let Ok(r) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RANGES, KEY_READ | KEY_WRITE) else {
        return (0, None);
    };
    let mut quitadas = 0;
    for ip in ips {
        let nombre = clave_vieja(ip);
        let es_nuestra = r.open_subkey(&nombre).ok().and_then(|k| k.get_value::<String, _>(":Range").ok()).map_or(false, |v| v == *ip);
        if es_nuestra && r.delete_subkey_all(&nombre).is_ok() {
            quitadas += 1;
        }
    }
    let ajena = r.enum_keys().flatten().find(|n| n.encode_utf16().count() >= NOMBRE_QUE_ROMPE);
    (quitadas, ajena)
}

// ── El arranque ──────────────────────────────────────────────────────────────────────────────

pub fn iniciar<R: Runtime>(app: &AppHandle<R>) {
    if !cfg!(windows) {
        return;
    }
    let marca = app.path().app_local_data_dir().ok().map(|d| d.join("carpetas-os.txt"));
    std::thread::Builder::new()
        .name("carpetas-os".into())
        .spawn(move || {
            std::thread::sleep(PRIMERA);
            let mut dicho: Option<String> = None;
            loop {
                let espera = match vuelta(marca.as_deref()) {
                    Ok(cambios) => {
                        poner(Estado::Listo);
                        if let Some(c) = cambios {
                            crate::salud::anotar("INFO", "carpetas-os", &c);
                        }
                        dicho = None;
                        CADA
                    }
                    Err(e) => {
                        poner(Estado::Fallo);
                        // El mismo fallo cada media hora no llena el diario: se anota cuando cambia.
                        if dicho.as_deref() != Some(e.as_str()) {
                            crate::salud::anotar("AVISO", "carpetas-os", &e);
                            dicho = Some(e);
                        }
                        REINTENTO
                    }
                };
                std::thread::sleep(espera);
            }
        })
        .ok();
}

#[cfg(not(windows))]
fn vuelta(_marca: Option<&Path>) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(windows)]
fn vuelta(marca: Option<&Path>) -> Result<Option<String>, String> {
    let pd = std::env::var_os("ProgramData").ok_or("no hay %ProgramData%")?;
    let _com = zonas::abrir_com();
    vuelta_con(&IPS, Path::new(&pd), marca)
}

/// Una vuelta entera. Ok(Some(texto)) si cambió algo en el computador (para el diario), Ok(None) si
/// todo estaba ya. Las zonas van primero: no dependen de la red.
#[cfg(windows)]
fn vuelta_con(ips: &[&str], program_data: &Path, marca: Option<&Path>) -> Result<Option<String>, String> {
    let mut cambios: Vec<String> = Vec::new();

    // 1. Los NAS como red local.
    let (quitadas, ajena) = limpiar_claves_viejas(ips);
    if quitadas > 0 {
        cambios.push(format!("{quitadas} entradas de zona viejas quitadas"));
        // Sin esto, un Explorador abierto seguiría con la lista rota hasta cerrar la sesión.
        zonas::refrescar();
    }
    let mut nuevas = 0;
    let mut rechazo: Option<String> = None;
    for ip in ips {
        // Si ya es de la red local (la puso alguien a mano, o una directiva), no se toca.
        if zona_local(zonas::de(ip)?) {
            continue;
        }
        match zonas::a_intranet(ip) {
            Ok(()) => nuevas += 1,
            Err(e) => rechazo = Some(e),
        }
    }
    // Lo que cuenta no es lo que se pidió sino lo que Windows contesta después.
    for ip in ips {
        let z = zonas::de(ip)?;
        if !zona_local(z) {
            let por = match (&rechazo, &ajena) {
                (Some(e), _) => format!(" {e}."),
                (None, Some(n)) => format!(" Hay una entrada de zona ajena de nombre largo («{n}») que hace que Windows ignore las siguientes."),
                (None, None) => " Una directiva del equipo manda sobre la del usuario.".to_string(),
            };
            return Err(format!("Windows sigue tratando a {ip} como Internet (zona {z}).{por}"));
        }
    }
    if nuevas > 0 {
        cambios.push(format!("{nuevas} NAS marcados como red local"));
    }

    // 1b. Que el Explorador guarde en el propio computador los desktop.ini de las carpetas de red
    // (11-nov-2026). Sin esto, cada vez que se abre una unidad del NAS el Explorador le pide al NAS el
    // desktop.ini de CADA carpeta pintada (10 ms en la oficina, 30-100 ms por ZeroTier). Es la
    // directiva documentada por Microsoft (KB 898612). Va en HKCU: no pide administrador.
    if poner_cache_desktop_ini() {
        cambios.push("caché de desktop.ini de red encendida".into());
    }

    // 2. Los iconos en ProgramData.
    let dir = carpeta_iconos(program_data);
    let falta = !dir.join(TESTIGO).exists();
    let ultima = marca.and_then(|m| std::fs::metadata(m).and_then(|m| m.modified()).ok());
    if toca_bajar(falta, ultima, SystemTime::now()) {
        let r = ureq::get(ICONOS_URL).timeout(Duration::from_secs(60)).call().map_err(|e| format!("no se pudieron bajar los iconos: {e}"))?;
        let mut paquete = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), MAX_BYTES), &mut paquete).map_err(|e| format!("no se pudieron bajar los iconos: {e}"))?;
        let iconos = iconos_del_tar(&paquete)?;
        let escritos = instalar_iconos(&dir, &iconos)?;
        if !dir.join(TESTIGO).exists() {
            return Err(format!("no quedaron los iconos en {}", dir.display()));
        }
        if let Some(m) = marca {
            if let Some(p) = m.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::write(m, b"ok");
        }
        if escritos > 0 {
            cambios.push(format!("{escritos} iconos instalados"));
        }
    }

    // 3. Que el Explorador vuelva a leer los iconos (los tenía guardados como carpeta amarilla).
    if cambios.is_empty() {
        return Ok(None);
    }
    refrescar_iconos();
    Ok(Some(cambios.join(", ")))
}

/// Pone `UseDesktopIniCache=1` en las directivas del Explorador del usuario. true si lo cambió.
#[cfg(windows)]
fn poner_cache_desktop_ini() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    const CLAVE: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(CLAVE) else {
        return false;
    };
    if k.get_value::<u32, _>("UseDesktopIniCache").ok() == Some(1) {
        return false;
    }
    k.set_value("UseDesktopIniCache", &1u32).is_ok()
}

/// ie4uinit.exe es de Windows y está firmado por Microsoft: es lo único que se lanza, sin ventana.
#[cfg(windows)]
fn refrescar_iconos() {
    use std::os::windows::process::CommandExt;
    let sistema = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32");
    let _ = std::process::Command::new(sistema.join("ie4uinit.exe"))
        .arg("-show")
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cabecera(nombre: &str, tam: usize, tipo: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..nombre.len()].copy_from_slice(nombre.as_bytes());
        let o = format!("{tam:011o}\0");
        h[124..136].copy_from_slice(o.as_bytes());
        h[156] = tipo;
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    fn tar(archivos: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut t = Vec::new();
        for (n, d, tipo) in archivos {
            t.extend(cabecera(n, d.len(), *tipo));
            t.extend_from_slice(d);
            t.extend(vec![0u8; (512 - d.len() % 512) % 512]);
        }
        t.extend(vec![0u8; 1024]);
        t
    }

    fn ico(relleno: u8) -> Vec<u8> {
        let mut d = vec![0, 0, 1, 0, 1, 0];
        d.extend(vec![relleno; 700]);
        d
    }

    #[test]
    fn las_claves_viejas_son_las_que_rompian_la_lista() {
        // Todas las que creaban la 1.36.0 y el comando del OS llegan al largo que detiene a Windows.
        for ip in IPS {
            assert!(clave_vieja(ip).len() >= NOMBRE_QUE_ROMPE, "{ip}");
        }
        assert_eq!(clave_vieja("10.61.5.27"), "Labstream 10.61.5.27");
    }

    #[test]
    fn internet_no_vale_y_lo_demas_si() {
        assert!(zona_local(0) && zona_local(1) && zona_local(2));
        assert!(!zona_local(3) && !zona_local(4));
    }

    #[test]
    fn baja_si_falta_o_una_vez_al_dia() {
        let ahora = SystemTime::now();
        assert!(toca_bajar(true, Some(ahora), ahora));
        assert!(toca_bajar(false, None, ahora));
        assert!(!toca_bajar(false, Some(ahora - Duration::from_secs(3600)), ahora));
        assert!(toca_bajar(false, Some(ahora - CADA), ahora));
        // Un reloj que va hacia atrás no hace bajar en cada vuelta.
        assert!(!toca_bajar(false, Some(ahora + Duration::from_secs(3600)), ahora));
    }

    #[test]
    fn solo_nombres_de_icono() {
        assert!(nombre_de_icono("os-64748b.ico"));
        for malo in ["os-64748B.ico", "os-64748.ico", "../os-64748b.ico", "os-64748b.exe", "C:\\x\\os-64748b.ico", "os-zzzzzz.ico", "desktop.ini", ""] {
            assert!(!nombre_de_icono(malo), "{malo}");
        }
    }

    #[test]
    fn abre_el_paquete_y_se_salta_lo_que_no_es_un_icono_nuestro() {
        let (a, b) = (ico(1), ico(2));
        let t = tar(&[("os-64748b.ico", &a, b'0'), ("leeme.txt", b"hola", b'0'), ("../os-000000.ico", &a, b'0'), ("os-0ea5e9.ico", &b, 0), ("os-111111.ico", &a, b'5')]);
        let iconos = iconos_del_tar(&t).unwrap();
        assert_eq!(iconos.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["os-64748b.ico", "os-0ea5e9.ico"]);
        assert_eq!(iconos[0].1, &a[..]);
        assert_eq!(iconos[1].1, &b[..]);
    }

    #[test]
    fn un_paquete_malo_no_instala_nada() {
        assert!(iconos_del_tar(b"<html>error</html>").is_err());
        assert!(iconos_del_tar(&[0u8; 2048]).is_err());
        // Un «icono» que no es un icono.
        assert!(iconos_del_tar(&tar(&[("os-64748b.ico", b"MZ este es un programa, no un icono....", b'0')])).is_err());
        // Cortado a la mitad.
        let entero = tar(&[("os-64748b.ico", &ico(1), b'0')]);
        assert!(iconos_del_tar(&entero[..700]).is_err());
        // Sin ningún icono.
        assert!(iconos_del_tar(&tar(&[("leeme.txt", b"hola", b'0')])).is_err());
    }

    #[test]
    fn instala_y_no_reescribe_lo_que_ya_esta_igual() {
        let dir = std::env::temp_dir().join(format!("labstream-carpetas-os-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (ico(1), ico(2));
        let juego = vec![("os-64748b.ico".to_string(), &a[..]), ("os-0ea5e9.ico".to_string(), &b[..])];
        assert_eq!(instalar_iconos(&dir, &juego).unwrap(), 2);
        assert_eq!(instalar_iconos(&dir, &juego).unwrap(), 0);
        // La paleta cambia un color: solo se escribe ese.
        let c = ico(3);
        let juego2 = vec![("os-64748b.ico".to_string(), &a[..]), ("os-0ea5e9.ico".to_string(), &c[..])];
        assert_eq!(instalar_iconos(&dir, &juego2).unwrap(), 1);
        assert_eq!(std::fs::read(dir.join("os-0ea5e9.ico")).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn los_iconos_van_a_program_data() {
        assert_eq!(carpeta_iconos(Path::new(r"C:\ProgramData")), PathBuf::from(r"C:\ProgramData\Labstream\carpetas-os"));
    }

    /// En este computador, de verdad: `PD=<carpeta de prueba> cargo test --lib carpetas_os -- --ignored`.
    /// Con una IP de documentación (192.0.2.x, que no existe en ninguna red) para no tocar las de los NAS:
    /// deja la clave vieja de nombre largo como la dejaba la 1.36.0, corre la vuelta y comprueba con
    /// Windows que la IP quedó como red local, que la clave vieja ya no está y que los iconos del OS
    /// bajaron a la carpeta de prueba. Al terminar (también si falla) quita lo que puso.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn de_punta_a_punta() {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        const IP: &str = "192.0.2.77";
        struct Limpia;
        impl Drop for Limpia {
            fn drop(&mut self) {
                zonas::quitar(IP);
                if let Ok(r) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RANGES, winreg::enums::KEY_ALL_ACCESS) {
                    let _ = r.delete_subkey_all(clave_vieja(IP));
                }
            }
        }
        let _com = zonas::abrir_com();
        let _limpia = Limpia;
        let pd = PathBuf::from(std::env::var("PD").expect("PD=<carpeta de prueba>"));
        let marca = pd.join("marca.txt");
        let ranges = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RANGES).unwrap().0;

        assert_eq!(zonas::de(IP).unwrap(), 3, "la IP de prueba empieza como Internet");
        // Como la dejaba la 1.36.0: una clave de 20 letras, que Windows no lee.
        let vieja = ranges.create_subkey(clave_vieja(IP)).unwrap().0;
        vieja.set_value(":Range", &IP).unwrap();
        vieja.set_value("file", &1u32).unwrap();

        let t0 = std::time::Instant::now();
        let dicho = vuelta_con(&[IP], &pd, Some(&marca)).expect("la vuelta").expect("cambió algo");
        println!("la vuelta tardó {:?}: {dicho}", t0.elapsed());
        assert!(dicho.contains("1 entradas de zona viejas quitadas") && dicho.contains("1 NAS marcados como red local"), "{dicho}");
        assert_eq!(zonas::de(IP).unwrap(), 1, "Windows ya la ve como red local");
        assert!(ranges.open_subkey(clave_vieja(IP)).is_err(), "la clave vieja ya no está");
        let dir = carpeta_iconos(&pd);
        assert!(dir.join(TESTIGO).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 45);
        assert!(marca.exists());

        // Pedir una que ya está: Windows no se queja y sigue siendo red local.
        println!("pedirla otra vez: {:?}", zonas::a_intranet(IP));
        assert_eq!(zonas::de(IP).unwrap(), 1);
        let nombres: Vec<String> = ranges.enum_keys().flatten().collect();
        println!("claves en Ranges durante la prueba: {nombres:?}");

        // La segunda vuelta, con todo puesto y la marca de hoy, no cambia nada.
        let t1 = std::time::Instant::now();
        assert_eq!(vuelta_con(&[IP], &pd, Some(&marca)).expect("la segunda vuelta"), None);
        println!("la segunda vuelta tardó {:?}", t1.elapsed());
    }
}
