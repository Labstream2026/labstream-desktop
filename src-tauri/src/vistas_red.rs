//! Las carpetas se abren como «Elementos generales» en el Explorador (1.38, 12-nov-2026).
//!
//! Munka: «siento el NAS lento en general» (10-nov). Al abrir una carpeta, el Explorador ADIVINA de qué
//! tipo es mirando lo que tiene dentro. Si ve vídeos la abre como «Vídeos», y entonces abre cada archivo
//! para sacar la duración, la fecha de grabación y la miniatura. En el NAS eso es leer por la red cada
//! clip (un .mov de cámara, un .braw…) solo para pintar la lista. Como «Elementos generales» enseña
//! nombre, fecha, tipo y tamaño, que vienen en el propio listado, y no abre ningún archivo.
//!
//! Lo que hace la app, solo en Windows, sin ventanas y sin administrador (todo es del usuario, HKCU):
//!   1. UNA VEZ POR COMPUTADOR pone `Bags\AllFolders\Shell\FolderType = NotSpecified`: el Explorador
//!      deja de adivinar y todas las carpetas que se abran de nuevo son «Elementos generales». Si ya
//!      había un valor (lo puso la persona u otra cosa), no se toca. Si alguien lo quita después, la app
//!      no lo vuelve a poner (para eso está la marca `vistas-generales.txt`).
//!   2. Mientras ese ajuste siga puesto, en cada vuelta (al abrir y una vez al día) olvida las vistas
//!      guardadas de CARPETAS DE RED que el Explorador ya tenía como vídeo, fotos o música: esas no se
//!      corrigen solas. Las demás vistas (las de este computador, y las de red que ya eran generales)
//!      no se tocan, así que nadie pierde cómo tenía sus carpetas.
//!
//! Lo que cambia para quien usa el computador: en las carpetas de fotos o vídeo ya no salen solas las
//! columnas «Duración» o «Fecha de captura»; las miniaturas siguen con «Ver › Iconos grandes». Las
//! carpetas especiales de Windows (Imágenes, Vídeos, Música, Descargas) tienen su tipo fijo. Los
//! colores de las carpetas del OS no dependen de esto (son el desktop.ini, ver carpetas_os.rs).
//!
//! El Explorador lee el ajuste al arrancar: cuenta del todo desde el siguiente inicio de sesión. No se
//! reinicia el Explorador desde aquí (cerraría las ventanas y cortaría una copia en marcha).
//!
//! Medido en el computador de Munka (10-nov-2026): de 3.578 vistas de carpetas del NAS, unas 50 eran
//! de vídeo, fotos o música (las de material grabado); con el ajuste, una carpeta con 5 .mov y 4 .mp4
//! se abrió sin adivinar nada.
//!
//! Cómo se sabe qué vista es de red sin tocar la red: BagMRU guarda la ruta de cada vista como una
//! cadena de trozos (ITEMIDLIST). Solo se le pide a Windows el nombre de los dos primeros trozos
//! («Red», «\\servidor» o «Este equipo › Z:\»), que no salen del computador; todo lo que cuelga de una
//! unidad de red o de «Red» es de red.

// En Mac solo queda `iniciar`, que no hace nada: lo demás es de Windows.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

/// Después de las zonas y los iconos (carpetas_os, a los 20 s): nada que corra.
const PRIMERA: Duration = Duration::from_secs(60);
const CADA: Duration = Duration::from_secs(24 * 3600);
/// Donde guarda el Explorador las vistas (HKCU).
pub(crate) const SHELL: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell";
pub(crate) const NO_ADIVINAR: &str = "NotSpecified";
const MARCA: &str = "vistas-generales.txt";

// ── Lo puro ──────────────────────────────────────────────────────────────────────────────────

/// Las plantillas que abren cada archivo para llenar sus columnas: Vídeos, Imágenes, Música.
const PLANTILLAS_QUE_LEEN: [&str; 3] = [
    "{5FA96407-7E77-483C-AC93-691D05850DE8}",
    "{B3690E58-E961-423B-B687-386EBFD83239}",
    "{94D6DDCC-4A68-4175-A374-BD584A510B78}",
];
/// Lo mismo, como lo apunta el Explorador cuando lo adivinó (`SniffedFolderType`).
const ADIVINADOS_QUE_LEEN: [&str; 3] = ["Videos", "Pictures", "Music"];
/// «Este equipo» y «Red», tal como los nombra Windows.
pub(crate) const ESTE_EQUIPO: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
pub(crate) const RED: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// ¿Esta vista guardada es de las que leen cada archivo? `plantillas`: las subclaves de `Bags\N\Shell`
/// (una por plantilla con la que se abrió); `adivinado`: su `SniffedFolderType`.
pub(crate) fn vista_que_lee(plantillas: &[String], adivinado: Option<&str>) -> bool {
    plantillas.iter().any(|p| PLANTILLAS_QUE_LEEN.iter().any(|q| q.eq_ignore_ascii_case(p)))
        || adivinado.is_some_and(|a| ADIVINADOS_QUE_LEEN.iter().any(|q| q.eq_ignore_ascii_case(a)))
}

/// Un trozo de ruta de BagMRU: sus 2 primeros bytes dicen cuánto mide (con ellos). Lo que sobre se
/// ignora; si no cuadra, no es un trozo.
pub(crate) fn trozo(valor: &[u8]) -> Option<&[u8]> {
    let cb = u16::from_le_bytes([*valor.first()?, *valor.get(1)?]) as usize;
    (cb > 2 && cb <= valor.len()).then(|| &valor[..cb])
}

/// Qué es el primer trozo de una ruta, por su nombre: de red entera, «Este equipo» (hay que mirar la
/// unidad) u otra cosa (escritorio, acceso rápido, OneDrive… nada de red que tocar).
#[derive(Debug, PartialEq)]
pub(crate) enum Raiz {
    DeRed,
    EsteEquipo,
    Otra,
}

pub(crate) fn raiz(nombre: &str) -> Raiz {
    if nombre.starts_with(r"\\") || nombre.eq_ignore_ascii_case(RED) {
        Raiz::DeRed
    } else if nombre.eq_ignore_ascii_case(ESTE_EQUIPO) {
        Raiz::EsteEquipo
    } else {
        Raiz::Otra
    }
}

/// La letra de una unidad tal como la nombra Windows («Z:\»), o None si no es una unidad.
pub(crate) fn unidad(nombre: &str) -> Option<String> {
    let b = nombre.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || &nombre[2..] == r"\"))
        .then(|| format!("{}:\\", b[0].to_ascii_uppercase() as char))
}

/// ¿Se pone el ajuste? Solo la primera vez (sin marca) y si no hay ya un valor de otro.
pub(crate) fn poner_ajuste(hay_marca: bool, valor: Option<&str>) -> bool {
    !hay_marca && valor.map_or(true, |v| v.trim().is_empty())
}

// ── El arranque ──────────────────────────────────────────────────────────────────────────────

pub fn iniciar<R: Runtime>(app: &AppHandle<R>) {
    if !cfg!(windows) {
        return;
    }
    let marca = app.path().app_local_data_dir().ok().map(|d| d.join(MARCA));
    std::thread::Builder::new()
        .name("vistas-red".into())
        .spawn(move || {
            std::thread::sleep(PRIMERA);
            let mut dicho: Option<String> = None;
            loop {
                match vuelta(SHELL, marca.as_deref()) {
                    Ok(Some(c)) => crate::salud::anotar("INFO", "vistas-red", &c),
                    Ok(None) => {}
                    Err(e) => {
                        if dicho.as_deref() != Some(e.as_str()) {
                            crate::salud::anotar("AVISO", "vistas-red", &e);
                            dicho = Some(e);
                        }
                    }
                }
                std::thread::sleep(CADA);
            }
        })
        .ok();
}

#[cfg(not(windows))]
fn vuelta(_shell: &str, _marca: Option<&Path>) -> Result<Option<String>, String> {
    Ok(None)
}

/// Una vuelta. Ok(Some(texto)) si cambió algo (para el diario de salud), Ok(None) si todo estaba ya.
/// `shell`: la rama del registro (la real, o una copia en las pruebas).
#[cfg(windows)]
fn vuelta(shell: &str, marca: Option<&Path>) -> Result<Option<String>, String> {
    let mut cambios: Vec<String> = Vec::new();
    let valor = registro::ajuste(shell);
    // Sin poder apuntar la marca no se pone nada: si no, se pondría en cada arranque aunque la persona
    // lo hubiera quitado.
    if let Some(marca) = marca {
        if poner_ajuste(marca.exists(), valor.as_deref()) {
            registro::poner_ajuste(shell)?;
            cambios.push("las carpetas se abren como «Elementos generales» (desde el próximo inicio de sesión)".into());
        }
        if !marca.exists() {
            if let Some(d) = marca.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(marca, "Las carpetas se pusieron como «Elementos generales» una vez. Si se quita el ajuste, la app no lo vuelve a poner.\n");
        }
    }
    if registro::ajuste(shell).as_deref() == Some(NO_ADIVINAR) {
        let olvidadas = registro::olvidar_vistas_de_red_que_leen(shell, true)?.len();
        if olvidadas > 0 {
            cambios.push(format!("{olvidadas} carpetas de red que se abrían como vídeo, fotos o música vuelven a generales"));
        }
    }
    Ok((!cambios.is_empty()).then(|| cambios.join(", ")))
}

#[cfg(windows)]
pub(crate) mod registro {
    use super::{raiz, trozo, unidad, vista_que_lee, Raiz, NO_ADIVINAR};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    /// El valor de `FolderType` para todas las carpetas, si hay.
    pub fn ajuste(shell: &str) -> Option<String> {
        hkcu().open_subkey(format!(r"{shell}\Bags\AllFolders\Shell")).ok()?.get_value::<String, _>("FolderType").ok()
    }

    pub fn poner_ajuste(shell: &str) -> Result<(), String> {
        let (k, _) = hkcu()
            .create_subkey(format!(r"{shell}\Bags\AllFolders\Shell"))
            .map_err(|e| format!("no se pudo abrir el ajuste de vistas: {e}"))?;
        k.set_value("FolderType", &NO_ADIVINAR).map_err(|e| format!("no se pudo poner «Elementos generales»: {e}"))
    }

    /// El nombre que da Windows a una ruta de trozos (solo se usa con los dos primeros: no sale a la red).
    fn nombre(pidl: &[u8]) -> Option<String> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::Common::ITEMIDLIST;
        use windows::Win32::UI::Shell::{SHGetNameFromIDList, SIGDN_DESKTOPABSOLUTEPARSING};
        let mut b = pidl.to_vec();
        b.extend_from_slice(&[0, 0]);
        let p = unsafe { SHGetNameFromIDList(b.as_ptr() as *const ITEMIDLIST, SIGDN_DESKTOPABSOLUTEPARSING) }.ok()?;
        let s = unsafe { p.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    fn es_unidad_de_red(raiz_unidad: &str) -> bool {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        const DRIVE_REMOTE: u32 = 4;
        unsafe { GetDriveTypeW(&HSTRING::from(raiz_unidad)) == DRIVE_REMOTE }
    }

    /// Los trozos hijos de un nodo de BagMRU: (nombre de su subclave, sus bytes).
    fn hijos(k: &RegKey) -> Vec<(String, Vec<u8>)> {
        k.enum_values()
            .flatten()
            .filter(|(n, _)| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
            .filter_map(|(n, v)| trozo(&v.bytes).map(|t| (n, t.to_vec())))
            .collect()
    }

    /// Los `NodeSlot` de un nodo y de todo lo que cuelga de él (sin pedir nombres: ya se sabe que es de red).
    fn ranuras(k: &RegKey, fuera: &mut Vec<u32>) {
        if let Ok(s) = k.get_value::<u32, _>("NodeSlot") {
            fuera.push(s);
        }
        for (n, _) in hijos(k) {
            if let Ok(h) = k.open_subkey(&n) {
                ranuras(&h, fuera);
            }
        }
    }

    /// COM abierto en este hilo mientras viva: sin él, Windows no da el nombre de ningún trozo.
    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
        }
    }

    /// Las ranuras (`Bags\N`) de todas las vistas de carpetas de red.
    pub fn ranuras_de_red(shell: &str) -> Result<Vec<u32>, String> {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let _com = Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
        let mru = hkcu().open_subkey(format!(r"{shell}\BagMRU")).map_err(|e| format!("no se pudo leer BagMRU: {e}"))?;
        let mut fuera = Vec::new();
        for (n, t) in hijos(&mru) {
            let Ok(k) = mru.open_subkey(&n) else { continue };
            match nombre(&t).map(|s| raiz(&s)) {
                Some(Raiz::DeRed) => ranuras(&k, &mut fuera),
                Some(Raiz::EsteEquipo) => {
                    for (n2, t2) in hijos(&k) {
                        let pidl = [t.as_slice(), t2.as_slice()].concat();
                        let de_red = nombre(&pidl).and_then(|s| unidad(&s)).is_some_and(|u| es_unidad_de_red(&u));
                        if de_red {
                            if let Ok(k2) = k.open_subkey(&n2) {
                                ranuras(&k2, &mut fuera);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(fuera)
    }

    /// Olvida (`aplicar`) o solo cuenta las vistas de red que abren cada archivo. Devuelve sus ranuras.
    pub fn olvidar_vistas_de_red_que_leen(shell: &str, aplicar: bool) -> Result<Vec<u32>, String> {
        let bags = hkcu()
            .open_subkey_with_flags(format!(r"{shell}\Bags"), KEY_READ | KEY_WRITE)
            .map_err(|e| format!("no se pudo abrir las vistas: {e}"))?;
        let mut olvidadas = Vec::new();
        for r in ranuras_de_red(shell)? {
            let Ok(sh) = bags.open_subkey(format!(r"{r}\Shell")) else { continue };
            let plantillas: Vec<String> = sh.enum_keys().flatten().collect();
            let adivinado = sh.get_value::<String, _>("SniffedFolderType").ok();
            if vista_que_lee(&plantillas, adivinado.as_deref()) && (!aplicar || bags.delete_subkey_all(r.to_string()).is_ok()) {
                olvidadas.push(r);
            }
        }
        Ok(olvidadas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_video_fotos_y_musica_leen_cada_archivo() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(vista_que_lee(&v(&["{5fa96407-7e77-483c-ac93-691d05850de8}"]), None), "vídeos, en minúsculas");
        assert!(vista_que_lee(&v(&["{5C4F28B5-F869-4E84-8E60-F11DB97C5CC7}"]), Some("Pictures")), "general, pero adivinada como fotos");
        assert!(vista_que_lee(&[], Some("Music")));
        assert!(!vista_que_lee(&v(&["{5C4F28B5-F869-4E84-8E60-F11DB97C5CC7}"]), Some("Generic")), "general");
        assert!(!vista_que_lee(&v(&["{7D49D726-3C21-4F05-99AA-FDC2C9474656}"]), Some("Documents")), "documentos no lee nada de más");
        assert!(!vista_que_lee(&[], None));
    }

    #[test]
    fn un_trozo_es_lo_que_dicen_sus_dos_primeros_bytes() {
        assert_eq!(trozo(&[5, 0, 0x2f, b'Z', b':', 0, 0]), Some(&[5u8, 0, 0x2f, b'Z', b':'][..]));
        assert_eq!(trozo(&[9, 0, 1]), None, "dice más de lo que hay");
        assert_eq!(trozo(&[2, 0]), None, "vacío");
        assert_eq!(trozo(&[7]), None);
    }

    #[test]
    fn la_red_y_las_unidades_por_su_nombre() {
        assert_eq!(raiz(r"\\192.168.0.22"), Raiz::DeRed);
        assert_eq!(raiz(r"\\192.168.0.22\labstream"), Raiz::DeRed);
        assert_eq!(raiz("::{f02c1a0d-be21-4350-88b0-7367fc96ef3c}"), Raiz::DeRed);
        assert_eq!(raiz(ESTE_EQUIPO), Raiz::EsteEquipo);
        assert_eq!(raiz(r"C:\Users\Munka\OneDrive"), Raiz::Otra);
        assert_eq!(raiz("::{679F85CB-0220-4080-B29B-5540CC05AAB6}"), Raiz::Otra, "acceso rápido");
        assert_eq!(unidad(r"z:\").as_deref(), Some(r"Z:\"));
        assert_eq!(unidad("M:").as_deref(), Some(r"M:\"));
        assert_eq!(unidad(r"Z:\Brigard"), None, "una carpeta, no la unidad");
        assert_eq!(unidad(ESTE_EQUIPO), None);
    }

    #[test]
    fn el_ajuste_se_pone_una_vez_y_no_pisa_otro() {
        assert!(poner_ajuste(false, None));
        assert!(poner_ajuste(false, Some("")));
        assert!(!poner_ajuste(true, None), "ya se puso una vez: si lo quitaron, no se vuelve a poner");
        assert!(!poner_ajuste(false, Some("Generic")), "ya hay uno de otro");
        assert!(!poner_ajuste(false, Some(NO_ADIVINAR)));
    }

    /// Contra una COPIA de las vistas del Explorador de este computador (no la real): la importa del
    /// respaldo del 10-nov a `HKCU\Software\Labstream-prueba-vistas` antes de correr. Ver
    /// scripts/probar-vistas-red.ps1.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn con_la_copia_del_registro() {
        const COPIA: &str = r"Software\Labstream-prueba-vistas\Shell";
        let red = registro::ranuras_de_red(COPIA).expect("BagMRU de la copia");
        let que_leen = registro::olvidar_vistas_de_red_que_leen(COPIA, false).unwrap();
        println!("vistas de red: {}, de vídeo/fotos/música: {}", red.len(), que_leen.len());
        assert!(!red.is_empty());
        let dir = std::env::temp_dir().join(format!("vistas-red-{}", std::process::id()));
        let marca = dir.join(MARCA);
        let dicho = vuelta(COPIA, Some(&marca)).unwrap();
        println!("primera vuelta: {dicho:?}");
        assert_eq!(registro::ajuste(COPIA).as_deref(), Some(NO_ADIVINAR));
        assert!(marca.exists());
        assert!(registro::olvidar_vistas_de_red_que_leen(COPIA, false).unwrap().is_empty(), "no queda ninguna");
        assert_eq!(registro::ranuras_de_red(COPIA).unwrap().len(), red.len(), "las rutas siguen: solo se olvidó la vista");
        assert_eq!(vuelta(COPIA, Some(&marca)).unwrap(), None, "la segunda vuelta no cambia nada");
        let _ = std::fs::remove_dir_all(dir);
    }
}
