//! Instalar una versión nueva sin que la app se quede cerrada (1.36.2, 10-nov-2026).
//!
//! Munka: «intenté abrir el OS en un equipo y se cierra de una vez» (un PC con NVIDIA; el 9-nov, lo
//! mismo en un portátil con AMD). Al abrir, la app busca versión nueva y, si la hay, la instala. En
//! Windows eso lo hacía el actualizador de Tauri así (tauri-plugin-updater 2.10, updater.rs
//! `install_inner`): baja el instalador, lo lanza con ShellExecuteW SIN mirar si Windows lo dejó
//! arrancar, y cierra la app con `process::exit(0)`. Si Windows bloquea el instalador (el Control
//! inteligente de aplicaciones con un programa sin firma, un antivirus) o el instalador no llega a
//! terminar, la app ya se cerró y nadie la vuelve a abrir. Y como al abrirla otra vez vuelve a
//! encontrar la versión nueva, se cierra otra vez: para quien la abre, «se cierra de una vez».
//!
//! Ahora:
//!   1. El instalador se lanza aquí, y solo se cierra la app si Windows lo dejó arrancar. Si no, la
//!      app sigue abierta, lo anota en el diario de salud con el motivo y lo avisa.
//!   2. Antes de cerrarse deja una marca con la versión que se instala. Al arrancar, si la marca dice
//!      una versión y la app es otra, la instalación no terminó: se anota y se avisa.
//!   3. En cualquiera de los dos casos, esa versión ya no se instala sola al abrir (sería cerrarse en
//!      cada arranque): la barra ofrece «Actualizar a vX» para intentarlo a mano. Una versión más
//!      nueva sí vuelve a intentarse sola.
//!   4. Al abrir, si va a instalar, lo dice antes de cerrarse: «se vuelve a abrir sola».
//!
//! En macOS no cambia nada: el actualizador reemplaza la app y la app se reinicia ella misma.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime};

/// La versión que se estaba instalando cuando la app se cerró para el instalador.
const MARCA: &str = "actualizando.txt";
/// La versión que falló: no se vuelve a instalar sola al abrir.
const NO_SOLA: &str = "actualizacion-fallida.txt";
/// Los argumentos que pone el actualizador de Tauri al instalador NSIS en modo pasivo (config.rs
/// `nsis_args` + `install_inner`): barra de progreso sin preguntas, y la app se vuelve a abrir.
#[cfg(windows)]
const ARGS_NSIS: [&str; 4] = ["/P", "/R", "/UPDATE", "/ARGS"];

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn dir() -> Option<PathBuf> {
    DIR.lock().ok().and_then(|d| d.clone())
}

// ── Lo puro ──────────────────────────────────────────────────────────────────────────────────

/// Lo que dice la marca del arranque anterior, comparado con la versión que arrancó.
#[derive(Debug, PartialEq)]
pub(crate) enum Arranque {
    /// No había instalación en curso.
    Normal,
    /// Se instaló bien: la app ya es esa versión.
    Instalada(String),
    /// La app se cerró para instalar esa versión y arrancó con otra: no terminó.
    NoTermino(String),
}

pub(crate) fn leer_arranque(marca: Option<&str>, actual: &str) -> Arranque {
    match marca.map(str::trim).filter(|v| !v.is_empty()) {
        None => Arranque::Normal,
        Some(v) if v == actual => Arranque::Instalada(v.to_string()),
        Some(v) => Arranque::NoTermino(v.to_string()),
    }
}

/// ¿Se puede instalar sola al abrir? No, si esa misma versión ya falló en este computador.
pub(crate) fn puede_sola(fallida: Option<&str>, nueva: &str) -> bool {
    fallida.map(str::trim) != Some(nueva)
}

/// El motivo, en claro, de que Windows no dejara arrancar el instalador (código de CreateProcess).
pub(crate) fn motivo(codigo: Option<i32>) -> &'static str {
    match codigo {
        // ERROR_APPLICATION_CONTROL / el bloqueo del Control inteligente de aplicaciones o de WDAC
        Some(4551) | Some(4556) | Some(1260) => "Windows lo bloqueó (Control inteligente de aplicaciones o una directiva del equipo): el instalador no lleva firma",
        Some(5) => "acceso denegado (suele ser un antivirus)",
        Some(225) => "Windows Defender lo marcó como peligroso",
        Some(740) => "pide permisos de administrador",
        Some(2) | Some(3) => "el instalador desapareció de la carpeta temporal (suele ser un antivirus)",
        _ => "Windows no dejó arrancar el instalador",
    }
}

// ── El arranque ──────────────────────────────────────────────────────────────────────────────

/// Va al principio del setup, después del diario de salud. Devuelve la versión que no terminó de
/// instalarse, si es el caso, para avisar.
pub fn preparar<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let d = app.path().app_local_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&d);
    if let Ok(mut g) = DIR.lock() {
        *g = Some(d.clone());
    }
    let marca = d.join(MARCA);
    let leido = std::fs::read_to_string(&marca).ok();
    let _ = std::fs::remove_file(&marca);
    match leer_arranque(leido.as_deref(), &app.package_info().version.to_string()) {
        Arranque::Normal => None,
        Arranque::Instalada(v) => {
            let _ = std::fs::remove_file(d.join(NO_SOLA));
            crate::salud::anotar("INFO", "actualizacion", &format!("quedó instalada la versión {v}"));
            None
        }
        Arranque::NoTermino(v) => {
            let _ = std::fs::write(d.join(NO_SOLA), &v);
            crate::salud::anotar("ERROR", "actualizacion", &format!("la instalación de la versión {v} no terminó: la app sigue en la {}", app.package_info().version));
            Some(v)
        }
    }
}

/// ¿Se instala sola al abrir esta versión?
pub fn puede_instalar_sola(version: &str) -> bool {
    let fallida = dir().and_then(|d| std::fs::read_to_string(d.join(NO_SOLA)).ok());
    puede_sola(fallida.as_deref(), version)
}

fn anotar_fallo(version: &str) {
    if let Some(d) = dir() {
        let _ = std::fs::write(d.join(NO_SOLA), version);
    }
}

// ── Instalar ─────────────────────────────────────────────────────────────────────────────────

/// Baja la versión (el actualizador comprueba su firma) y la instala. En Windows, si Windows dejó
/// arrancar el instalador, la app se cierra aquí mismo y no vuelve; si no, devuelve el motivo y la
/// app sigue abierta. En macOS devuelve Ok y quien llama reinicia.
pub async fn instalar(update: tauri_plugin_updater::Update) -> Result<(), String> {
    let version = update.version.clone();
    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(b) => b,
        Err(e) => return Err(format!("no se pudo bajar la versión {version}: {e}")),
    };
    #[cfg(windows)]
    {
        // Un .exe (MZ) es el instalador NSIS: lo lanza la app. Otra cosa (un .msi), como siempre.
        if bytes.starts_with(b"MZ") {
            let r = lanzar_nsis(&version, &bytes);
            if r.is_err() {
                anotar_fallo(&version);
            }
            return r;
        }
    }
    marcar(&version);
    crate::salud::antes_de_actualizar(&version);
    update.install(&bytes).map_err(|e| {
        quitar_marca();
        anotar_fallo(&version);
        format!("no se pudo instalar la versión {version}: {e}")
    })
}

fn marcar(version: &str) {
    if let Some(d) = dir() {
        let _ = std::fs::write(d.join(MARCA), version);
    }
}

fn quitar_marca() {
    if let Some(d) = dir() {
        let _ = std::fs::remove_file(d.join(MARCA));
    }
}

#[cfg(windows)]
fn lanzar_nsis(version: &str, bytes: &[u8]) -> Result<(), String> {
    let ruta = std::env::temp_dir().join(format!("labstream-os-{version}-setup.exe"));
    std::fs::write(&ruta, bytes).map_err(|e| format!("no se pudo guardar el instalador de la versión {version}: {e}"))?;
    lanzar(&ruta, version)
}

#[cfg(windows)]
fn lanzar(ruta: &Path, version: &str) -> Result<(), String> {
    // La marca va ANTES de lanzar: si el instalador arranca, puede cerrar la app en cualquier momento.
    marcar(version);
    match std::process::Command::new(ruta).args(ARGS_NSIS).spawn() {
        Ok(_) => {
            crate::salud::antes_de_actualizar(version);
            std::process::exit(0);
        }
        Err(e) => {
            quitar_marca();
            let _ = std::fs::remove_file(ruta);
            Err(format!("no se pudo instalar la versión {version}: {} ({e})", motivo(e.raw_os_error())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_marca_dice_si_la_instalacion_termino() {
        assert_eq!(leer_arranque(None, "1.36.2"), Arranque::Normal);
        assert_eq!(leer_arranque(Some("  "), "1.36.2"), Arranque::Normal);
        assert_eq!(leer_arranque(Some("1.36.2\n"), "1.36.2"), Arranque::Instalada("1.36.2".into()));
        assert_eq!(leer_arranque(Some("1.36.3"), "1.36.2"), Arranque::NoTermino("1.36.3".into()));
    }

    #[test]
    fn una_version_que_fallo_no_se_instala_sola_pero_una_mas_nueva_si() {
        assert!(puede_sola(None, "1.36.3"));
        assert!(!puede_sola(Some("1.36.3"), "1.36.3"));
        assert!(!puede_sola(Some("1.36.3\n"), "1.36.3"));
        assert!(puede_sola(Some("1.36.3"), "1.36.4"));
    }

    #[test]
    fn cada_bloqueo_se_explica() {
        assert!(motivo(Some(4551)).contains("Control inteligente"));
        assert!(motivo(Some(5)).contains("antivirus"));
        assert!(motivo(None).contains("no dejó arrancar"));
    }

    /// Lo que pasa cuando Windows no deja arrancar el instalador: la app NO se cierra (si se cerrara,
    /// esta prueba no terminaría), devuelve el motivo y la versión queda como fallida.
    #[cfg(windows)]
    #[test]
    fn si_windows_no_deja_arrancar_el_instalador_la_app_sigue_abierta() {
        let d = std::env::temp_dir().join(format!("labstream-act-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        *DIR.lock().unwrap() = Some(d.clone());
        // Un «instalador» que Windows no puede arrancar (no es un programa).
        let ruta = d.join("no-es-un-programa-setup.exe");
        std::fs::write(&ruta, b"MZ esto no es un programa de verdad").unwrap();
        let r = lanzar(&ruta, "9.9.9");
        let e = r.expect_err("no tenía que poder arrancar");
        assert!(e.contains("9.9.9"), "{e}");
        assert!(!d.join(MARCA).exists(), "sin marca: no hay instalación en curso");
        anotar_fallo("9.9.9");
        assert!(!puede_instalar_sola("9.9.9"));
        assert!(puede_instalar_sola("9.9.10"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
