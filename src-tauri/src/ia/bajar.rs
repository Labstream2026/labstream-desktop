// ── Bajar las piezas de la IA: comprobadas con su huella y, si vienen en un zip, sacadas de él ──
//
// Todo va a la carpeta de la IA en los datos de la app del equipo (`ia::carpeta`). Cada pieza se
// baja a un `.parte`, se le saca la huella SHA-256 MIENTRAS llega y solo si cuadra con la anotada
// en el código pasa a su sitio; al acabar se deja una marca `<nombre>.ok` con la huella, que es lo
// que se mira después para saber si está (un archivo a medias nunca tiene marca).

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Una pieza que se baja: un archivo tal cual o un zip del que se sacan unos archivos.
pub struct Pieza {
    /// Nombre corto (la marca `.ok` y los avisos).
    pub nombre: &'static str,
    pub url: &'static str,
    /// Huella SHA-256 del archivo bajado, en hexadecimal.
    pub sha256: &'static str,
    pub bytes: u64,
    pub forma: Forma,
    /// Se pide con la sesión de la pestaña (lo sirve el servidor de la app solo al equipo).
    pub con_sesion: bool,
}

pub enum Forma {
    /// El archivo, con este nombre, en la carpeta de la IA.
    Archivo(&'static str),
    /// Un zip (un .nupkg lo es): de él, estos archivos —por su ruta EXACTA dentro: un paquete trae
    /// varias DLL con el mismo nombre (x86, arm64, Xbox…)— a la carpeta de la IA, con su nombre.
    Zip(&'static [&'static str]),
    /// Un zip entero (el motor generativo, el modelo SDXL: miles de archivos) a la carpeta de la IA,
    /// con sus carpetas; su carpeta de arriba tiene que llamarse así.
    Carpeta(&'static str),
}

fn marca(dir: &Path, p: &Pieza) -> PathBuf {
    dir.join(format!("{}.ok", p.nombre))
}

/// ¿Está ya en el equipo (con su marca y la huella de ahora)?
pub fn esta(dir: &Path, p: &Pieza) -> bool {
    fs::read_to_string(marca(dir, p)).map(|s| s.trim() == p.sha256).unwrap_or(false)
}

/// Baja la pieza (si no está) avisando de lo que lleva con `avance(bytes_nuevos)`. `sesion`: las
/// cookies de la pestaña para el servidor de la app (solo viajan a él: `con_sesion` y su origen).
#[cfg_attr(not(test), allow(dead_code))]
pub fn bajar(dir: &Path, p: &Pieza, sesion: Option<&str>, avance: &mut dyn FnMut(u64)) -> Result<(), String> {
    bajar_con(dir, p, sesion, avance, &mut |_| {})
}

/// Como `bajar`, avisando también de cuánto va el sacar un zip entero (`sacando(0..1)`).
pub fn bajar_con(dir: &Path, p: &Pieza, sesion: Option<&str>, avance: &mut dyn FnMut(u64), sacando: &mut dyn FnMut(f64)) -> Result<(), String> {
    if esta(dir, p) {
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(|e| format!("No se pudo crear la carpeta de la IA: {e}"))?;
    let parte = dir.join(format!("{}.parte", p.nombre));
    let mut pedido = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build()
        .get(p.url);
    if p.con_sesion && p.url.starts_with(&format!("{}/", crate::SERVER_URL)) {
        if let Some(c) = sesion.filter(|c| !c.is_empty()) {
            pedido = pedido.set("Cookie", c);
        }
    }
    let resp = pedido.call().map_err(|e| match e {
        ureq::Error::Status(401, _) => format!("No se pudo bajar {}: la sesión de la app caducó (vuelve a entrar).", p.nombre),
        e => format!("No se pudo bajar {}: {e}", p.nombre),
    })?;
    let mut leer = resp.into_reader();
    let mut escribir = BufWriter::new(File::create(&parte).map_err(|e| format!("No se pudo escribir {}: {e}", p.nombre))?);
    let mut huella = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total: u64 = 0;
    loop {
        let n = leer.read(&mut buf).map_err(|e| format!("La descarga de {} se cortó: {e}", p.nombre))?;
        if n == 0 {
            break;
        }
        huella.update(&buf[..n]);
        escribir.write_all(&buf[..n]).map_err(|e| format!("No se pudo escribir {}: {e}", p.nombre))?;
        total += n as u64;
        avance(n as u64);
    }
    escribir.flush().map_err(|e| format!("No se pudo escribir {}: {e}", p.nombre))?;
    drop(escribir);
    let hex = huella.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if hex != p.sha256 {
        let _ = fs::remove_file(&parte);
        return Err(format!("{} llegó distinto de lo esperado ({total} bytes). Vuelve a intentarlo.", p.nombre));
    }
    match p.forma {
        Forma::Archivo(destino) => {
            let d = dir.join(destino);
            let _ = fs::remove_file(&d);
            fs::rename(&parte, &d).map_err(|e| format!("No se pudo guardar {}: {e}", p.nombre))?;
        }
        Forma::Zip(quiero) => {
            let r = extraer(&parte, dir, quiero);
            let _ = fs::remove_file(&parte);
            r?;
        }
        Forma::Carpeta(carpeta) => {
            let _ = fs::remove_dir_all(dir.join(carpeta));
            let r = extraer_todo(&parte, dir, carpeta, sacando);
            let _ = fs::remove_file(&parte);
            r?;
        }
    }
    fs::write(marca(dir, p), p.sha256).map_err(|e| format!("No se pudo guardar {}: {e}", p.nombre))?;
    Ok(())
}

/// Saca del zip los archivos de `quiero` (rutas exactas dentro del zip) a `dir`, con su nombre.
fn extraer(zip: &Path, dir: &Path, quiero: &[&str]) -> Result<(), String> {
    let f = File::open(zip).map_err(|e| format!("No se pudo abrir lo bajado: {e}"))?;
    let mut z = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| format!("Lo bajado no es un zip válido: {e}"))?;
    let mut faltan: Vec<&str> = quiero.to_vec();
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| format!("El zip está dañado: {e}"))?;
        if e.is_dir() {
            continue;
        }
        let ruta = e.name().replace('\\', "/");
        let nombre = ruta.rsplit('/').next().unwrap_or("").to_string();
        if nombre.is_empty() || nombre.contains("..") {
            continue;
        }
        if let Some(k) = faltan.iter().position(|q| q.eq_ignore_ascii_case(&ruta)) {
            faltan.remove(k);
            let destino = dir.join(&nombre);
            let tmp = dir.join(format!("{nombre}.parte"));
            let mut out = BufWriter::new(File::create(&tmp).map_err(|e| format!("No se pudo escribir {nombre}: {e}"))?);
            io::copy(&mut e, &mut out).map_err(|e| format!("No se pudo sacar {nombre}: {e}"))?;
            out.flush().map_err(|e| format!("No se pudo escribir {nombre}: {e}"))?;
            drop(out);
            let _ = fs::remove_file(&destino);
            fs::rename(&tmp, &destino).map_err(|e| format!("No se pudo guardar {nombre}: {e}"))?;
        }
    }
    if faltan.is_empty() {
        Ok(())
    } else {
        Err(format!("En lo bajado no vinieron: {}", faltan.join(", ")))
    }
}

/// Saca el zip entero a `dir`: solo lo que cuelga de `carpeta/`, con sus rutas (sin «..» ni
/// absolutas), avisando cada ~1 % de las entradas.
fn extraer_todo(zip: &Path, dir: &Path, carpeta: &str, sacando: &mut dyn FnMut(f64)) -> Result<(), String> {
    let f = File::open(zip).map_err(|e| format!("No se pudo abrir lo bajado: {e}"))?;
    let mut z = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| format!("Lo bajado no es un zip válido: {e}"))?;
    let total = z.len().max(1);
    let mut sacados = 0usize;
    let mut ultimo = 0usize;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| format!("El zip está dañado: {e}"))?;
        let ruta = e.name().replace('\\', "/");
        let Some(resto) = ruta.strip_prefix(&format!("{carpeta}/")) else { continue };
        if resto.is_empty() || resto.split('/').any(|t| t.is_empty() || t == "." || t == "..") || ruta.starts_with('/') {
            continue;
        }
        let destino = dir.join(carpeta).join(resto);
        if e.is_dir() {
            fs::create_dir_all(&destino).map_err(|x| format!("No se pudo crear {resto}: {x}"))?;
            continue;
        }
        if let Some(padre) = destino.parent() {
            fs::create_dir_all(padre).map_err(|x| format!("No se pudo crear {}: {x}", padre.display()))?;
        }
        let mut out = BufWriter::new(File::create(&destino).map_err(|x| format!("No se pudo escribir {resto}: {x}"))?);
        io::copy(&mut e, &mut out).map_err(|x| format!("No se pudo sacar {resto}: {x}"))?;
        out.flush().map_err(|x| format!("No se pudo escribir {resto}: {x}"))?;
        sacados += 1;
        if i - ultimo > total / 100 {
            ultimo = i;
            sacando(i as f64 / total as f64);
        }
    }
    if sacados == 0 {
        return Err(format!("En lo bajado no venía la carpeta {carpeta}."));
    }
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn saca_un_zip_entero_a_su_carpeta_sin_salirse() {
        let dir = std::env::temp_dir().join(format!("ls-ia-prueba-todo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zp = dir.join("m.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            w.start_file("motor/python/python.exe", o).unwrap();
            w.write_all(b"PY").unwrap();
            w.start_file("motor/python/Lib/x.py", o).unwrap();
            w.write_all(b"X").unwrap();
            w.start_file("motor/../fuera.txt", o).unwrap();
            w.write_all(b"NO").unwrap();
            w.start_file("otra/cosa.txt", o).unwrap();
            w.write_all(b"NO").unwrap();
            w.finish().unwrap();
        }
        let mut avisos = 0;
        extraer_todo(&zp, &dir, "motor", &mut |_| avisos += 1).unwrap();
        assert_eq!(fs::read(dir.join("motor/python/python.exe")).unwrap(), b"PY");
        assert_eq!(fs::read(dir.join("motor/python/Lib/x.py")).unwrap(), b"X");
        assert!(!dir.join("fuera.txt").exists() && !dir.join("otra").exists());
        assert!(extraer_todo(&zp, &dir, "nada", &mut |_| {}).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saca_del_zip_solo_lo_pedido_y_dice_lo_que_falta() {
        let dir = std::env::temp_dir().join(format!("ls-ia-prueba-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zp = dir.join("x.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            w.start_file("bin/x64-xbox/a.dll", o).unwrap();
            w.write_all(b"XBOX").unwrap();
            w.start_file("bin/x64-win/a.dll", o).unwrap();
            w.write_all(b"AAA").unwrap();
            w.start_file("bin/x64-win/b.dll", o).unwrap();
            w.write_all(b"BBB").unwrap();
            w.finish().unwrap();
        }
        // la de la ruta pedida, no la primera con ese nombre
        extraer(&zp, &dir, &["bin/x64-win/a.dll"]).unwrap();
        assert_eq!(fs::read(dir.join("a.dll")).unwrap(), b"AAA");
        assert!(!dir.join("b.dll").exists());
        assert!(extraer(&zp, &dir, &["a.dll"]).unwrap_err().contains("a.dll"));
        let p = Pieza { nombre: "x", url: "", sha256: "abc", bytes: 1, forma: Forma::Archivo("x"), con_sesion: false };
        assert!(!esta(&dir, &p));
        fs::write(marca(&dir, &p), "abc").unwrap();
        assert!(esta(&dir, &p));
        let _ = fs::remove_dir_all(&dir);
    }
}
