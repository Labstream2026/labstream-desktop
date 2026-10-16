// ── El motor GENERATIVO (1.27, 15-oct-2026): SDXL Inpainting en un proceso aparte ──
//
// Munka: «quiero que solo funcione el modelo más potente para relleno de imágenes». Rellenar con
// IA ya no es LaMa en la CPU: es Stable Diffusion XL Inpainting (fp16) en la tarjeta NVIDIA, que
// crea lo que se le pide con una frase dentro de lo pintado (o quita lo pintado y sigue la escena).
// Corre en un proceso aparte —Python 3.12 embebido con PyTorch CUDA 12.8 y diffusers, el zip del
// motor de piezas.rs— con `servidor.py` (este mismo archivo va dentro de la app: include_str!) que
// escucha en 127.0.0.1 en el puerto que el sistema le dé; esta app lo arranca cuando hace falta,
// le habla por HTTP y lo apaga al salir (y él se cierra solo si la app muere).
//
// 1.29 (motor v4): la página precarga el modelo al abrir Rellenar (`abrir_modelo`) y lo mantiene
// mientras el panel está abierto (`mantener`); vive en la RAM y sube a la tarjeta por partes solo al
// generar. Las propuestas salen de una en una y con el color corregido (servidor.py); para quitar,
// el motor arranca del hueco tapado con su borde y el grano de alrededor (`prellenar`).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SERVIDOR_PY: &str = include_str!("servidor.py");
/// La carpeta del motor y la del modelo dentro de la carpeta de la IA (lo que traen los zips).
pub const CARPETA_MOTOR: &str = "motor-generativo";
pub const CARPETA_MODELO: &str = "sdxl-inpaint";
/// Lo que se espera a que el motor arranque y al modelo abrirse (la primera vez lee 7 GB del disco).
const ARRANQUE: Duration = Duration::from_secs(90);
const RELLENO: Duration = Duration::from_secs(20 * 60);

pub struct Motor {
    hijo: Child,
    pub puerto: u16,
    pub usado: Instant,
}

static MOTOR: Mutex<Option<Motor>> = Mutex::new(None);

fn base(puerto: u16) -> String {
    format!("http://127.0.0.1:{puerto}")
}

/// ¿El motor contesta? (y si el modelo está abierto)
fn salud(puerto: u16) -> Option<serde_json::Value> {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build().get(&format!("{}/salud", base(puerto))).call().ok()?.into_json().ok()
}

/// El motor en marcha (lo arranca si no está): su puerto.
pub fn arrancar(dir: &Path) -> Result<u16, String> {
    let mut m = MOTOR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(x) = m.as_mut() {
        if x.hijo.try_wait().ok().flatten().is_none() && salud(x.puerto).is_some() {
            x.usado = Instant::now();
            return Ok(x.puerto);
        }
        let _ = x.hijo.kill();
        *m = None;
    }
    let motor = dir.join(CARPETA_MOTOR);
    let python = motor.join("python").join("python.exe");
    if !python.exists() {
        return Err("El motor generativo no está en el equipo.".into());
    }
    // servidor.py, siempre el de esta versión de la app
    let guion = motor.join("servidor.py");
    std::fs::write(&guion, SERVIDOR_PY).map_err(|e| format!("No se pudo preparar el motor: {e}"))?;
    let mut cmd = Command::new(&python);
    cmd.arg("-I").arg(&guion).arg("0").arg(dir.join(CARPETA_MODELO)).arg(std::process::id().to_string()).current_dir(&motor).env("PYTHONIOENCODING", "utf-8").stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: sin consola
    }
    let mut hijo = cmd.spawn().map_err(|e| format!("No se pudo arrancar el motor generativo: {e}"))?;
    let salida = hijo.stdout.take().ok_or("El motor arrancó sin salida.")?;
    let mut lector = BufReader::new(salida);
    let mut linea = String::new();
    let t0 = Instant::now();
    lector.read_line(&mut linea).map_err(|e| format!("El motor no dijo su puerto: {e}"))?;
    let puerto: u16 = linea.trim().strip_prefix("puerto ").and_then(|p| p.parse().ok()).ok_or_else(|| {
        let _ = hijo.kill();
        format!("El motor no arrancó bien ({}).", linea.trim())
    })?;
    // el resto de lo que diga, al registro de la app (línea a línea, aunque alguna no sea UTF-8:
    // si este lector muriera, la tubería se cerraría y el motor no podría ni escribir)
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match lector.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let l = String::from_utf8_lossy(&bytes);
                    let l = l.trim_end();
                    if !l.is_empty() {
                        crate::salud::anotar("INFO", "ia", &format!("motor: {l}"));
                    }
                }
            }
        }
    });
    while salud(puerto).is_none() {
        if t0.elapsed() > ARRANQUE {
            let _ = hijo.kill();
            return Err("El motor generativo no respondió al arrancar.".into());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    crate::salud::anotar("INFO", "ia", &format!("motor generativo en marcha en {:.1} s (puerto {puerto})", t0.elapsed().as_secs_f32()));
    *m = Some(Motor { hijo, puerto, usado: Instant::now() });
    Ok(puerto)
}

/// Lo que se espera a que el modelo se abra: la primera vez lee 7 GB del disco (de la RAM, segundos).
const ABRIR: Duration = Duration::from_secs(4 * 60);

/// La precarga (1.29): el motor en marcha y el modelo ya abierto (en la RAM; a la tarjeta sube por
/// partes al generar), para que el primer relleno no espere. La página la pide al abrir Rellenar (por ia_preparar). Un motor de antes de la v4 no
/// sabe abrirlo así: entonces se abre con el primer relleno, como siempre.
pub fn abrir_modelo(dir: &Path) -> Result<(), String> {
    let puerto = arrancar(dir)?;
    let t0 = Instant::now();
    match ureq::AgentBuilder::new().timeout(ABRIR).build().post(&format!("{}/abrir", base(puerto))).send_string("{}") {
        Ok(_) => {
            if let Some(x) = MOTOR.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                x.usado = Instant::now();
            }
            crate::salud::anotar("INFO", "ia", &format!("generativo: modelo listo en {:.1} s", t0.elapsed().as_secs_f32()));
            Ok(())
        }
        Err(ureq::Error::Status(404, _)) => Ok(()),
        Err(ureq::Error::Status(_, r)) => {
            let e = r.into_json::<serde_json::Value>().ok().and_then(|v| v.get("error").and_then(|x| x.as_str()).map(String::from)).unwrap_or_else(|| "el motor falló".into());
            Err(format!("No se pudo abrir el modelo: {e}"))
        }
        Err(e) => Err(format!("El motor generativo no contestó: {e}")),
    }
}

/// «Sigue haciendo falta» (1.29): mientras la página tiene Rellenar abierto, el modelo no se suelta.
/// No arranca nada: si el motor no está en marcha, no hay nada que mantener. ¿Lo había?
pub fn mantener() -> bool {
    let puerto = {
        let mut m = MOTOR.lock().unwrap_or_else(|e| e.into_inner());
        let Some(x) = m.as_mut() else {
            return false;
        };
        if x.hijo.try_wait().ok().flatten().is_some() {
            return false;
        }
        x.usado = Instant::now();
        x.puerto
    };
    ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build().post(&format!("{}/mantener", base(puerto))).send_string("{}").is_ok()
}

/// Lo que se le pide al motor.
pub struct Pedido<'a> {
    pub rgba: &'a [u8],
    pub ancho: usize,
    pub alto: usize,
    pub frase: String,
    pub n: u8,
    pub pasos: u8,
    pub semilla: Option<u64>,
    /// cuánto se aparta de lo que había dentro de lo pintado (0,1..1; 1 = lo inventa del todo)
    pub fuerza: f32,
    /// cuánto pesa la frase (1..15)
    pub guia: f32,
    /// para quitar (1.29): el hueco tapado con su borde y el grano de alrededor antes de generar
    pub prellenar: bool,
}

/// Rellena lo pintado: `n` propuestas de RGB (`ancho` × `alto`) y la semilla que se usó.
pub fn rellenar(dir: &Path, p: &Pedido) -> Result<(Vec<Vec<u8>>, u64), String> {
    use base64::Engine;
    let puerto = arrancar(dir)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut cuerpo = serde_json::json!({ "rgba": b64.encode(p.rgba), "ancho": p.ancho, "alto": p.alto, "frase": p.frase, "n": p.n, "pasos": p.pasos, "fuerza": p.fuerza, "guia": p.guia });
    if let Some(s) = p.semilla {
        cuerpo["semilla"] = serde_json::json!(s);
    }
    if p.prellenar {
        cuerpo["prellenar"] = serde_json::json!(true);
    }
    let t0 = Instant::now();
    let r = ureq::AgentBuilder::new().timeout(RELLENO).build().post(&format!("{}/rellenar", base(puerto))).send_json(cuerpo);
    let v: serde_json::Value = match r {
        Ok(r) => r.into_json().map_err(|e| format!("El motor contestó algo raro: {e}"))?,
        Err(ureq::Error::Status(_, r)) => {
            let e = r.into_json::<serde_json::Value>().ok().and_then(|v| v.get("error").and_then(|x| x.as_str()).map(String::from)).unwrap_or_else(|| "el motor falló".into());
            return Err(format!("No se pudo rellenar: {e}"));
        }
        Err(e) => return Err(format!("El motor generativo no contestó: {e}")),
    };
    let imagenes = v.get("imagenes").and_then(|x| x.as_array()).ok_or("El motor no devolvió imágenes.")?;
    let mut salida = Vec::with_capacity(imagenes.len());
    for im in imagenes {
        let bytes = b64.decode(im.as_str().unwrap_or("")).map_err(|_| "Una propuesta llegó dañada.".to_string())?;
        if bytes.len() != p.ancho * p.alto * 3 {
            return Err("Una propuesta llegó con otro tamaño.".into());
        }
        salida.push(bytes);
    }
    let semilla = v.get("semilla").and_then(|x| x.as_u64()).unwrap_or(0);
    if let Some(x) = MOTOR.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        x.usado = Instant::now();
    }
    crate::salud::anotar("INFO", "ia", &format!("generativo: {} propuestas de {}×{} a {} pasos · fuerza {:.2} · guía {:.1}{} en {:.1} s", salida.len(), p.ancho, p.alto, p.pasos, p.fuerza, p.guia, if p.prellenar { " · prellenado" } else { "" }, t0.elapsed().as_secs_f32()));
    Ok((salida, semilla))
}

/// Apaga el motor (al salir de la app).
pub fn apagar() {
    let mut m = MOTOR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut x) = m.take() {
        let _ = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build().post(&format!("{}/salir", base(x.puerto))).send_string("{}");
        std::thread::sleep(Duration::from_millis(300));
        let _ = x.hijo.kill();
    }
}

/// Los restos de la IA de antes (LaMa, 1.23–1.26): ya no se usan.
pub fn limpiar_lama(dir: &Path) {
    for n in ["lama-g-ronimo-44bb92b2.onnx", "lama-g-ronimo-44bb92b2.ok"] {
        let _ = std::fs::remove_file(dir.join(n));
    }
}
