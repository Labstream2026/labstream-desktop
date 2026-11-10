//! Los colores de las carpetas del OS en el Explorador, solos (1.36, 10-nov-2026).
//!
//! Las carpetas que el OS asigna a un cliente llevan en el NAS un desktop.ini que las pinta del color
//! del cliente con «OS» (Labstream OS, lib/carpetas-os). Windows 11 solo lo aplica si:
//!   1. el NAS cuenta como red local: por IP (\\192.168.0.22) Windows lo trata como «Internet» y no
//!      personaliza sus carpetas. Cada IP va a la zona «Intranet local» del usuario (HKCU, sin
//!      administrador), con las MISMAS claves que pone el comando de Win+R del OS
//!      (`Ranges\Labstream <ip>`, `:Range` = ip, `file` = 1);
//!   2. el icono está en el propio computador, y en %ProgramData%: desde AppData el Explorador no lo
//!      carga (probado en 26H2). El juego de iconos (uno por color de la paleta) se baja del OS
//!      (`/api/conector-unidades/iconos.tar`, público: solo dibujos de colores) y se saca con el
//!      tar.exe que Windows ya trae.
//!
//! Antes eso era un comando que cada persona pegaba en Windows + R desde el aviso del Inicio, y en el
//! primer computador costó varios tropiezos. Munka: «quiero que sea muy fácil de implementar». Ahora lo
//! hace la app sola al abrir, sin preguntar y sin ventanas: 20 s después de arrancar, y otra vez cada
//! día (por si la paleta trae colores nuevos o alguien borró los iconos). Si no hay red, se reintenta a
//! la media hora. Nada de esto conecta unidades ni da acceso a ninguna carpeta.
//!
//! La página del Inicio pregunta `carpetas_os_estado`: en esta app el aviso del comando ya no sale,
//! salvo que aquí haya fallado (entonces queda el comando como reserva).

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

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Estado {
    /// No es Windows: en Mac las carpetas no se pintan así.
    NoAplica,
    /// Todavía no terminó la primera vuelta.
    Preparando,
    Listo,
    /// La última vuelta no pudo dejar los iconos (sin red, sin permiso…): el Inicio enseña el comando.
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

pub(crate) fn clave_zona(ip: &str) -> String {
    format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings\ZoneMap\Ranges\Labstream {ip}")
}

/// ¿Toca bajar los iconos? Si falta el testigo, siempre; si no, una vez al día.
pub(crate) fn toca_bajar(falta_testigo: bool, ultima: Option<SystemTime>, ahora: SystemTime) -> bool {
    falta_testigo || ultima.map_or(true, |u| ahora.duration_since(u).map_or(false, |d| d >= CADA))
}

/// Un tar de verdad: la firma «ustar» va en el byte 257 de la primera cabecera.
pub(crate) fn parece_tar(b: &[u8]) -> bool {
    b.len() >= 512 && &b[257..262] == b"ustar"
}

pub(crate) fn carpeta_iconos(program_data: &Path) -> PathBuf {
    program_data.join("Labstream").join("carpetas-os")
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
            loop {
                let espera = match vuelta(marca.as_deref()) {
                    Ok(()) => {
                        poner(Estado::Listo);
                        CADA
                    }
                    Err(e) => {
                        poner(Estado::Fallo);
                        crate::salud::anotar("AVISO", "carpetas-os", &e);
                        REINTENTO
                    }
                };
                std::thread::sleep(espera);
            }
        })
        .ok();
}

#[cfg(windows)]
fn oculto(programa: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new(programa);
    c.creation_flags(0x0800_0000) // CREATE_NO_WINDOW: sin consola
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c
}

#[cfg(not(windows))]
fn vuelta(_marca: Option<&Path>) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn vuelta(marca: Option<&Path>) -> Result<(), String> {
    let sistema = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32");
    let mut cambio = false;

    // 1. Los NAS como red local (solo las IP que falten).
    for ip in IPS {
        let clave = clave_zona(ip);
        let ya = oculto(&sistema.join("reg.exe").to_string_lossy()).args(["query", &clave, "/v", ":Range"]).status().map_or(false, |s| s.success());
        if ya {
            continue;
        }
        let a = oculto(&sistema.join("reg.exe").to_string_lossy()).args(["add", &clave, "/v", ":Range", "/t", "REG_SZ", "/d", ip, "/f"]).status();
        let b = oculto(&sistema.join("reg.exe").to_string_lossy()).args(["add", &clave, "/v", "file", "/t", "REG_DWORD", "/d", "1", "/f"]).status();
        if !(a.map_or(false, |s| s.success()) && b.map_or(false, |s| s.success())) {
            return Err(format!("no se pudo marcar {ip} como red local"));
        }
        cambio = true;
    }

    // 2. Los iconos en ProgramData.
    let pd = std::env::var_os("ProgramData").ok_or("no hay %ProgramData%")?;
    let dir = carpeta_iconos(Path::new(&pd));
    let falta = !dir.join(TESTIGO).exists();
    let ultima = marca.and_then(|m| std::fs::metadata(m).and_then(|m| m.modified()).ok());
    if toca_bajar(falta, ultima, SystemTime::now()) {
        std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
        let r = ureq::get(ICONOS_URL).timeout(Duration::from_secs(60)).call().map_err(|e| format!("no se pudieron bajar los iconos: {e}"))?;
        let mut datos = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), MAX_BYTES), &mut datos).map_err(|e| format!("no se pudieron bajar los iconos: {e}"))?;
        if !parece_tar(&datos) {
            return Err("lo que llegó no es el paquete de iconos".into());
        }
        let tmp = std::env::temp_dir().join(format!("labstream-iconos-{}.tar", std::process::id()));
        std::fs::write(&tmp, &datos).map_err(|e| format!("no se pudo guardar el paquete: {e}"))?;
        // Si otro usuario de este computador ya los instaló, sus archivos no se pueden reescribir: tar
        // falla en esos y no importa, ya están. Lo que cuenta es el testigo.
        let _ = oculto(&sistema.join("tar.exe").to_string_lossy()).arg("-xf").arg(&tmp).arg("-C").arg(&dir).status();
        let _ = std::fs::remove_file(&tmp);
        if !dir.join(TESTIGO).exists() {
            return Err(format!("no quedaron los iconos en {}", dir.display()));
        }
        if let Some(m) = marca {
            if let Some(p) = m.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::write(m, b"ok");
        }
        if falta {
            cambio = true;
            crate::salud::anotar("INFO", "carpetas-os", "iconos de las carpetas del OS instalados");
        }
    }

    // 3. Que el Explorador vuelva a leer los iconos sin cerrar la sesión.
    if cambio {
        let _ = oculto(&sistema.join("ie4uinit.exe").to_string_lossy()).arg("-show").status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_claves_son_las_del_comando_del_os() {
        assert_eq!(clave_zona("192.168.0.22"), r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings\ZoneMap\Ranges\Labstream 192.168.0.22");
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
    fn solo_acepta_un_tar() {
        let mut t = vec![0u8; 1024];
        t[257..262].copy_from_slice(b"ustar");
        assert!(parece_tar(&t));
        assert!(!parece_tar(b"<html>error</html>"));
        assert!(!parece_tar(&[0u8; 1024]));
    }

    #[test]
    fn los_iconos_van_a_program_data() {
        assert_eq!(carpeta_iconos(Path::new(r"C:\ProgramData")), PathBuf::from(r"C:\ProgramData\Labstream\carpetas-os"));
    }

    /// En este computador, de verdad: `PD=<carpeta de prueba> cargo test --lib carpetas_os -- --ignored`.
    /// Baja el paquete del OS a esa carpeta (como si fuera ProgramData) y marca los NAS como red local.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn de_punta_a_punta() {
        let pd = std::env::var("PD").expect("PD=<carpeta de prueba>");
        std::env::set_var("ProgramData", &pd);
        let marca = Path::new(&pd).join("marca.txt");
        vuelta(Some(&marca)).expect("la vuelta");
        let dir = carpeta_iconos(Path::new(&pd));
        assert!(dir.join(TESTIGO).exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 45);
        assert!(marca.exists());
        for ip in IPS {
            assert!(oculto("reg.exe").args(["query", &clave_zona(ip), "/v", "file"]).status().unwrap().success(), "{ip}");
        }
        // La segunda vuelta, con todo puesto y la marca de hoy, no baja nada.
        let antes = std::fs::metadata(dir.join(TESTIGO)).unwrap().modified().unwrap();
        vuelta(Some(&marca)).expect("la segunda vuelta");
        assert_eq!(std::fs::metadata(dir.join(TESTIGO)).unwrap().modified().unwrap(), antes);
    }
}
