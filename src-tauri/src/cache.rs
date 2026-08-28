// ── El «cajón» de miniaturas de la app ──
//
// El problema que resuelve: las webviews guardan las imágenes en un caché del SISTEMA que es
// pequeño y volátil — el sistema lo vacía cuando necesita espacio. En un Mac del equipo se
// midieron 22 MB; un solo set de 500 fotos, entre cuadrícula y visor, pasa de 150 MB. No cabe,
// se borra solo, y al reabrir la app hay que volver a bajarlo todo del NAS. Eso es lo que se
// siente como «se relee cada vez que cierro».
//
// El cajón es un caché PROPIO de la app, en su carpeta interna (`app_cache_dir`, invisible en el
// Finder), con un tope que decidimos nosotros y una poda por antigüedad — igual que la que el NAS
// ya hace con sus miniaturas. Sobrevive a cerrar la app y se vacía con un botón.
//
// Cómo se usa: la web app, DENTRO de la app de escritorio, pide las miniaturas por el esquema
// `lsthumb://` en vez de directamente al servidor (lo reescribe el script de inicio). Este módulo
// es quien contesta ese esquema: si la miniatura está en el cajón la sirve del disco sin tocar la
// red; si no, la trae del NAS una vez, la guarda y la sirve.
//
// La clave del cajón NO incluye el token firmado de la URL (que rota cada 24 h): se deriva de la
// identidad estable de la miniatura (la ruta + su variante), así que la misma foto acierta en el
// cajón un mes después. El token solo se usa para ir a buscarla al NAS cuando falta.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager, Runtime};

// El único servidor del que este cajón acepta traer bytes. Sin este cerrojo, una URL manipulada
// convertiría la app en un proxy abierto que trae cualquier cosa de internet (un SSRF). Se compara
// por ORIGEN exacto (esquema + host + puerto), no por «empieza por».
const SERVER: &str = "https://os.labstreamsas.com";

// Tope del cajón: 3 GB, una sesión grande entera de miniaturas. Al pasarse, se borra lo más viejo
// (por último uso) hasta bajar del tope. Es el mismo criterio y tamaño que el caché del NAS.
const CAP_BYTES: u64 = 3 * 1024 * 1024 * 1024;

// Recorrer el directorio para podar es caro: no se hace en cada escritura, sino cada N.
const PODA_CADA: u32 = 200;
static DESDE_PODA: AtomicU32 = AtomicU32::new(0);

// Subcarpeta del caché de la app donde vive el cajón.
const DIR: &str = "miniaturas";

// ── Piezas PURAS (probadas abajo) ─────────────────────────────────────────────

// ¿Se puede traer esta URL? Solo el origen del servidor, y solo rutas de su API. Cualquier otra
// cosa se rechaza: el cajón no es un proxy de propósito general.
pub fn permitido(url: &str) -> bool {
    let (Ok(u), Ok(srv)) = (tauri::Url::parse(url), tauri::Url::parse(SERVER)) else {
        return false;
    };
    u.origin() == srv.origin() && u.path().starts_with("/api/")
}

// La clave estable de una miniatura: su ruta + los parámetros que la IDENTIFICAN, ordenados, y
// SIN el token `t` (que rota). Se resume con FNV-1a a un nombre corto y seguro para el sistema de
// archivos. Que no lleve el token es lo que hace que el cajón siga acertando cuando el token de la
// página cambió: la foto es la misma.
pub fn clave_de(url: &str) -> Option<String> {
    let u = tauri::Url::parse(url).ok()?;
    let mut params: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != "t") // el token NO forma parte de la identidad
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();
    let mut base = u.path().to_string();
    for (k, v) in &params {
        base.push('|');
        base.push_str(k);
        base.push('=');
        base.push_str(v);
    }
    Some(fnv1a_hex(&base))
}

// Saca el parámetro `u` (la URL real del servidor, percent-encoded) de la URI del esquema. Funciona
// sea cual sea la forma que la plataforma le dé al esquema — `lsthumb://c/?u=…` en macOS,
// `http://lsthumb.localhost/…?u=…` en Windows —: en ambas `u` viaja como parámetro de consulta y
// query_pairs lo decodifica.
pub fn url_pedida(uri: &str) -> Option<String> {
    tauri::Url::parse(uri)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "u")
        .map(|(_, v)| v.into_owned())
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

// Un archivo del cajón, para decidir la poda.
#[derive(Clone)]
struct Entrada {
    ruta: PathBuf,
    bytes: u64,
    usado: SystemTime,
}

// La POLÍTICA de poda, aislada del disco para poder probarla: dado el contenido del cajón y el
// tope, devuelve qué archivos borrar — los MÁS VIEJOS por último uso, hasta que el total baje del
// tope. Si ya cabe, no borra nada.
fn plan_de_poda(mut items: Vec<Entrada>, cap: u64) -> Vec<PathBuf> {
    let total: u64 = items.iter().map(|e| e.bytes).sum();
    if total <= cap {
        return Vec::new();
    }
    // Más viejo primero (el primero en salir).
    items.sort_by_key(|e| e.usado);
    let mut sobra = total - cap;
    let mut a_borrar = Vec::new();
    for e in items {
        if sobra == 0 {
            break;
        }
        a_borrar.push(e.ruta);
        sobra = sobra.saturating_sub(e.bytes);
    }
    a_borrar
}

// ── Disco ──────────────────────────────────────────────────────────────────────

fn dir<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    let base = app.path().app_cache_dir().ok()?;
    Some(base.join(DIR))
}

// Lee una miniatura del cajón. `None` = no está (o no se pudo leer): el que llama irá al NAS.
pub fn leer<R: Runtime>(app: &AppHandle<R>, clave: &str) -> Option<Vec<u8>> {
    let ruta = dir(app)?.join(clave);
    fs::read(&ruta).ok()
}

// Guarda una miniatura en el cajón (escritura atómica: se escribe a un temporal y se renombra, así
// una lectura nunca ve un archivo a medias). Cada cierto número de escrituras, poda.
pub fn escribir<R: Runtime>(app: &AppHandle<R>, clave: &str, bytes: &[u8]) {
    let Some(d) = dir(app) else { return };
    if fs::create_dir_all(&d).is_err() {
        return;
    }
    let destino = d.join(clave);
    let tmp = d.join(format!("{clave}.tmp"));
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(&tmp, &destino);
    }
    if DESDE_PODA.fetch_add(1, Ordering::Relaxed) + 1 >= PODA_CADA {
        DESDE_PODA.store(0, Ordering::Relaxed);
        podar(&d);
    }
}

fn podar(d: &Path) {
    let Ok(rd) = fs::read_dir(d) else { return };
    let mut items = Vec::new();
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let nombre = e.file_name();
        if nombre.to_string_lossy().ends_with(".tmp") {
            continue; // un temporal a medio escribir no se poda ni se cuenta
        }
        // Último uso: la fecha de acceso si el sistema la lleva, si no la de modificación.
        let usado = meta.accessed().or_else(|_| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        items.push(Entrada { ruta: e.path(), bytes: meta.len(), usado });
    }
    for ruta in plan_de_poda(items, CAP_BYTES) {
        let _ = fs::remove_file(ruta);
    }
}

// Cuánto ocupa el cajón, en bytes (para la etiqueta «Vaciar caché (NN MB)» del menú).
pub fn tamano<R: Runtime>(app: &AppHandle<R>) -> u64 {
    let Some(d) = dir(app) else { return 0 };
    let Ok(rd) = fs::read_dir(&d) else { return 0 };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

// Vacía el cajón entero. No pierde nada importante: se vuelve a llenar solo con el uso.
pub fn vaciar<R: Runtime>(app: &AppHandle<R>) {
    if let Some(d) = dir(app) {
        let _ = fs::remove_dir_all(&d);
        let _ = fs::create_dir_all(&d);
    }
}

// ── Traer del NAS (solo cuando falta en el cajón) ───────────────────────────────

// Tope de lo que se acepta bajar por miniatura. Una miniatura webp pesa cientos de KB; 64 MB es un
// techo absurdo a propósito, solo para que una respuesta descomunal no llene la memoria.
const MAX_DESCARGA: u64 = 64 * 1024 * 1024;

// Trae los bytes de una miniatura del servidor. Devuelve `(bytes, es_webp)`: solo se GUARDAN en el
// cajón las que son webp (lo único que este esquema pide); una respuesta de otro tipo se sirve tal
// cual pero no se cachea. `Err(codigo)` = no se pudo traer (para que la imagen quede rota como
// quedaría sin la app, no en blanco).
pub fn traer(url: &str) -> Result<(Vec<u8>, bool), u16> {
    let resp = match ureq::get(url).timeout(Duration::from_secs(30)).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(code),
        Err(_) => return Err(502), // sin red o error de transporte
    };
    let es_webp = resp
        .header("Content-Type")
        .map(|c| c.starts_with("image/webp"))
        .unwrap_or(false);
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_DESCARGA)
        .read_to_end(&mut bytes)
        .map_err(|_| 502u16)?;
    Ok((bytes, es_webp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn url_pedida_saca_y_decodifica_la_url_real() {
        let real = "https://os.labstreamsas.com/api/files-asset/abc?t=1.x&thumb=1";
        let uri = format!("lsthumb://c/?u={}", urlencoding_min(real));
        assert_eq!(url_pedida(&uri).as_deref(), Some(real));
        // Y también la forma que podría tomar en Windows.
        let uri_win = format!("http://lsthumb.localhost/?u={}", urlencoding_min(real));
        assert_eq!(url_pedida(&uri_win).as_deref(), Some(real));
    }

    #[test]
    fn url_pedida_sin_u_es_none() {
        assert_eq!(url_pedida("lsthumb://c/?otra=cosa"), None);
    }

    // Percent-encoding mínimo para las pruebas (: / ? & =), suficiente para estos casos.
    fn urlencoding_min(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                ':' => "%3A".into(),
                '/' => "%2F".into(),
                '?' => "%3F".into(),
                '&' => "%26".into(),
                '=' => "%3D".into(),
                c => c.to_string(),
            })
            .collect()
    }

    #[test]
    fn permitido_solo_el_servidor_y_su_api() {
        assert!(permitido("https://os.labstreamsas.com/api/files-asset/abc?t=1.x&thumb=1"));
        // Otro host, aunque diga labstream: NO.
        assert!(!permitido("https://malo.labstreamsas.com.attacker.net/api/x"));
        // El servidor pero fuera de /api/: NO (nada de rutas arbitrarias).
        assert!(!permitido("https://os.labstreamsas.com/etc/passwd"));
        // http en vez de https: distinto origen, NO.
        assert!(!permitido("http://os.labstreamsas.com/api/x"));
        // Basura: NO.
        assert!(!permitido("no-soy-una-url"));
    }

    #[test]
    fn la_clave_ignora_el_token_que_rota() {
        // La MISMA miniatura con dos tokens distintos (mañana, pasado) → la misma clave.
        let hoy = clave_de("https://os.labstreamsas.com/api/files-asset/abc?t=1000.aaa&thumb=1");
        let man = clave_de("https://os.labstreamsas.com/api/files-asset/abc?t=2000.bbb&thumb=1");
        assert!(hoy.is_some());
        assert_eq!(hoy, man, "el token no debe formar parte de la clave");
    }

    #[test]
    fn la_clave_distingue_foto_y_variante() {
        let mini = clave_de("https://os.labstreamsas.com/api/files-asset/abc?t=1.x&thumb=1").unwrap();
        let xl = clave_de("https://os.labstreamsas.com/api/files-asset/abc?t=1.x&thumb=xl").unwrap();
        let otra = clave_de("https://os.labstreamsas.com/api/files-asset/def?t=1.x&thumb=1").unwrap();
        assert_ne!(mini, xl, "la miniatura y el visor son distintos");
        assert_ne!(mini, otra, "dos fotos distintas son claves distintas");
    }

    #[test]
    fn la_clave_no_depende_del_orden_de_los_parametros() {
        let a = clave_de("https://os.labstreamsas.com/api/files-asset/abc?thumb=1&t=1.x").unwrap();
        let b = clave_de("https://os.labstreamsas.com/api/files-asset/abc?t=1.x&thumb=1").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn la_clave_es_segura_para_el_sistema_de_archivos() {
        let k = clave_de("https://os.labstreamsas.com/api/files-asset/abc?thumb=1").unwrap();
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()), "clave = {k}");
    }

    fn e(ruta: &str, bytes: u64, hace_segundos: u64) -> Entrada {
        Entrada {
            ruta: PathBuf::from(ruta),
            bytes,
            usado: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - hace_segundos),
        }
    }

    #[test]
    fn poda_no_borra_nada_si_cabe() {
        let items = vec![e("a", 100, 10), e("b", 100, 5)];
        assert!(plan_de_poda(items, 1000).is_empty());
    }

    #[test]
    fn poda_borra_lo_mas_viejo_primero_hasta_bajar_del_tope() {
        // Total 900, tope 500 → sobran 400. Debe borrar los más viejos hasta cubrir 400.
        let items = vec![
            e("nueva", 300, 1),    // la más reciente
            e("media", 300, 100),
            e("vieja", 300, 1000), // la más antigua
        ];
        let borra = plan_de_poda(items, 500);
        // Vieja (300) no basta (faltan 100) → también media. Nueva se salva.
        assert_eq!(borra, vec![PathBuf::from("vieja"), PathBuf::from("media")]);
    }

    #[test]
    fn poda_justo_en_el_tope_no_borra() {
        let items = vec![e("a", 250, 10), e("b", 250, 5)];
        assert!(plan_de_poda(items, 500).is_empty(), "500==500 cabe");
    }
}
