// ── La IA EN EL EQUIPO: la máscara de sujeto (1.22) y Rellenar (1.23) de Labstream OS ──────────
//
// Munka: «los pcs que usan esta función tienen Nvidia de 16 GB… el modelo se ejecuta en local y se
// conecta y sube el resultado a la app». En el editor de revelado (modo edición de un set):
// - «Sujeto» y «Fondo» los busca esta app: ONNX Runtime con DirectML sobre la tarjeta NVIDIA y
//   BiRefNet en media precisión (piezas.rs): 0,3 s por foto en vez de 2,5–3 s en el navegador.
// - «Rellenar» (Eliminar con IA): LaMa rellena lo pintado (relleno.rs), en la CPU (motor.rs:
//   abrir_en_cpu; LaMa no corre con DirectML).
//
// La página (lib/revelado/ia-local en labstream-os) llama a cuatro comandos, permitidos SOLO para el
// origen de la app (capabilities/default.json + el manifiesto de build.rs):
//   ia_estado   ({ que }) → ¿se puede aquí?, ¿está ya bajado?, cuánto falta, qué tarjeta
//   ia_preparar ({ que }) → baja lo que falte (eventos «ia-progreso» { fase, parte } a esa pestaña)
//                           y abre el modelo
//   ia_sujeto   ← RGBA en el cuerpo (binario) + x-ancho / x-alto
//               → [ancho u32][alto u32][un byte por punto] (lib/revelado/ia-local: leerRespuesta)
//   ia_rellenar ← RGBA con el hueco en el alfa + x-ancho / x-alto
//               → [ancho u32][alto u32][RGB, 3 bytes por punto]
// `que` es "sujeto" (o nada: la página de la 1.22) o "relleno". El resultado vuelve a la página, que lo
// pone en la máscara o en un parche de la foto y lo guarda con su revelado.
//
// Nada se baja ni se carga al arrancar la app: solo cuando alguien lo pide. Lo bajado vive en los
// datos de la app del equipo (`carpeta`), comprobado con su huella (bajar.rs). Cada modelo abierto
// ocupa memoria (el del sujeto, ~7 GB de la tarjeta mientras busca: DirectML guarda lo que pidió):
// se suelta a los SOLTAR_TRAS sin usarlo, que los editores trabajan con Resolve abierto.

mod bajar;
mod imagen;
mod motor;
mod piezas;
mod relleno;
mod tarjeta;

use piezas::Que;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, EventTarget, Manager, Runtime, Webview};

/// Sin usarlo este rato, el modelo se suelta (de la tarjeta, o de la memoria el de rellenar).
const SOLTAR_TRAS: Duration = Duration::from_secs(2 * 60);

struct Abierto {
    motor: motor::Motor,
    usado: Instant,
}

/// El modelo abierto de cada IA: el del sujeto (DirectML) y el de rellenar (CPU).
static SUJETO: Mutex<Option<Abierto>> = Mutex::new(None);
static RELLENO: Mutex<Option<Abierto>> = Mutex::new(None);
/// Una preparación a la vez (dos pestañas que piden a la vez no bajan dos veces).
static PREPARANDO: Mutex<()> = Mutex::new(());
static VIGILANTE: std::sync::Once = std::sync::Once::new();

fn abierto(que: Que) -> &'static Mutex<Option<Abierto>> {
    match que {
        Que::Sujeto => &SUJETO,
        Que::Relleno => &RELLENO,
    }
}

fn carpeta<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path().app_local_data_dir().map(|d| d.join("ia")).map_err(|e| format!("No hay carpeta de datos de la app: {e}"))
}

fn faltan(dir: &std::path::Path, que: Que) -> u64 {
    piezas::de(que).into_iter().filter(|p| !bajar::esta(dir, p)).map(|p| p.bytes).sum()
}

#[derive(serde::Serialize)]
pub struct Estado {
    /// de qué IA habla (así la página sabe que esta app conoce Rellenar)
    que: &'static str,
    disponible: bool,
    motivo: Option<String>,
    listo: bool,
    faltan: u64,
    modelo: &'static str,
    tarjeta: Option<String>,
}

/// Por qué esta IA no puede correr aquí (None: sí puede). El sujeto pide NVIDIA; rellenar va en la CPU.
fn motivo(que: Que, tarjeta: Option<&tarjeta::Tarjeta>) -> Option<String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        Some("La IA del equipo es para la app de Windows.".to_string())
    } else if que == Que::Sujeto && tarjeta.is_none() {
        Some("Este equipo no tiene tarjeta NVIDIA.".to_string())
    } else {
        None
    }
}

#[tauri::command]
pub fn ia_estado<R: Runtime>(app: AppHandle<R>, que: Option<String>) -> Estado {
    let que = Que::de(que.as_deref());
    let t = tarjeta::nvidia();
    let motivo = motivo(que, t.as_ref());
    let dir = carpeta(&app).ok();
    let falta = dir.as_deref().map(|d| faltan(d, que)).unwrap_or(u64::MAX);
    Estado {
        que: que.nombre(),
        disponible: motivo.is_none() && dir.is_some(),
        motivo,
        listo: falta == 0,
        faltan: if falta == u64::MAX { 0 } else { falta },
        modelo: match que {
            Que::Sujeto => piezas::NOMBRE_MODELO,
            Que::Relleno => piezas::NOMBRE_RELLENO,
        },
        tarjeta: t.map(|t| t.nombre),
    }
}

fn avisar<R: Runtime>(app: &AppHandle<R>, label: &str, fase: &str, parte: f64) {
    let _ = app.emit_to(EventTarget::webview(label), "ia-progreso", serde_json::json!({ "fase": fase, "parte": parte }));
}

/// Baja lo que falte y deja el modelo abierto (en un hilo aparte: puede tardar minutos).
#[tauri::command]
pub async fn ia_preparar<R: Runtime>(app: AppHandle<R>, webview: Webview<R>, que: Option<String>) -> Result<(), String> {
    let que = Que::de(que.as_deref());
    let label = webview.label().to_string();
    // La sesión de la pestaña, para bajar el modelo del servidor de la app (solo viaja a él). Aquí y
    // no en el hilo de abajo: leer cookies de WebView2 desde un comando síncrono se queda colgado.
    let sesion = tauri::Url::parse(crate::SERVER_URL)
        .ok()
        .and_then(|u| webview.cookies_for_url(u).ok())
        .map(|cs| cs.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; "));
    tauri::async_runtime::spawn_blocking(move || preparar(&app, &label, sesion.as_deref(), que))
        .await
        .map_err(|e| format!("La IA no pudo prepararse: {e}"))?
}

fn preparar<R: Runtime>(app: &AppHandle<R>, label: &str, sesion: Option<&str>, que: Que) -> Result<(), String> {
    if let Some(m) = motivo(que, tarjeta::nvidia().as_ref()) {
        return Err(m);
    }
    let _uno = PREPARANDO.lock().unwrap_or_else(|e| e.into_inner());
    let dir = carpeta(app)?;
    let total = faltan(&dir, que);
    if total > 0 {
        let hecho = AtomicU64::new(0);
        let mut ultimo = 0u64;
        avisar(app, label, "bajando", 0.0);
        for p in piezas::de(que) {
            bajar::bajar(&dir, p, sesion, &mut |n| {
                let h = hecho.fetch_add(n, Ordering::Relaxed) + n;
                // un aviso cada ~1 %
                if h - ultimo > total / 100 {
                    ultimo = h;
                    avisar(app, label, "bajando", (h as f64 / total as f64).min(1.0));
                }
            })
            .map_err(|e| {
                crate::salud::anotar("ERROR", "ia", &e);
                e
            })?;
        }
        crate::salud::anotar("INFO", "ia", &format!("IA de {} bajada al equipo ({} MB)", que.nombre(), total / 1_000_000));
    }
    let mut m = abierto(que).lock().unwrap_or_else(|e| e.into_inner());
    if m.is_none() {
        avisar(app, label, "preparando", 0.0);
        let t0 = Instant::now();
        let motor = match que {
            Que::Sujeto => motor::Motor::abrir(&dir),
            Que::Relleno => motor::Motor::abrir_en_cpu(&dir, piezas::ARCHIVO_RELLENO),
        }
        .map_err(|e| {
            crate::salud::anotar("ERROR", "ia", &format!("no se pudo abrir el modelo de {}: {e}", que.nombre()));
            e
        })?;
        crate::salud::anotar("INFO", "ia", &format!("modelo de {} abierto en {:.1} s ({})", que.nombre(), t0.elapsed().as_secs_f32(), motor.donde()));
        *m = Some(Abierto { motor, usado: Instant::now() });
        vigilar();
    }
    Ok(())
}

/// Suelta cada modelo cuando lleva SOLTAR_TRAS sin usarse.
fn vigilar() {
    VIGILANTE.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(15));
            for que in [Que::Sujeto, Que::Relleno] {
                let mut m = abierto(que).lock().unwrap_or_else(|e| e.into_inner());
                if m.as_ref().map(|a| a.usado.elapsed() >= SOLTAR_TRAS).unwrap_or(false) {
                    *m = None;
                    crate::salud::anotar("INFO", "ia", &format!("modelo de {} soltado (sin uso)", que.nombre()));
                }
            }
        });
    });
}

/// La foto que manda la página: RGBA en el cuerpo, medidas en las cabeceras.
fn foto_de(request: &tauri::ipc::Request<'_>) -> Result<(Vec<u8>, usize, usize), String> {
    let medida = |k: &str| request.headers().get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let (ancho, alto) = (medida("x-ancho"), medida("x-alto"));
    let tauri::ipc::InvokeBody::Raw(rgba) = request.body() else {
        return Err("La foto no llegó (se esperaban sus píxeles).".into());
    };
    if ancho < 2 || alto < 2 || ancho > 16_384 || alto > 16_384 || rgba.len() != ancho * alto * 4 {
        return Err("La foto llegó con unas medidas que no cuadran.".into());
    }
    Ok((rgba.clone(), ancho, alto))
}

/// Busca el sujeto en la foto que manda la página.
#[tauri::command]
pub async fn ia_sujeto(request: tauri::ipc::Request<'_>) -> Result<tauri::ipc::Response, String> {
    let (rgba, ancho, alto) = foto_de(&request)?;
    tauri::async_runtime::spawn_blocking(move || buscar(&rgba, ancho, alto))
        .await
        .map_err(|e| format!("La IA falló: {e}"))?
        .map(tauri::ipc::Response::new)
}

fn buscar(rgba: &[u8], ancho: usize, alto: usize) -> Result<Vec<u8>, String> {
    let mut m = SUJETO.lock().unwrap_or_else(|e| e.into_inner());
    let Some(a) = m.as_mut() else {
        return Err("La IA no está preparada.".into());
    };
    let lado = piezas::LADO;
    let t0 = Instant::now();
    let salida = a.motor.correr(imagen::entrada(rgba, ancho, alto, lado), lado);
    a.usado = Instant::now();
    match salida {
        Ok(s) => {
            crate::salud::anotar("INFO", "ia", &format!("sujeto en {:.2} s", t0.elapsed().as_secs_f32()));
            Ok(imagen::respuesta(lado as u32, lado as u32, &imagen::a_bytes(&s, imagen::son_logits(&s))))
        }
        Err(e) => {
            // una tarjeta que se perdió: se suelta para abrirla de nuevo la próxima vez
            *m = None;
            crate::salud::anotar("ERROR", "ia", &format!("no se pudo buscar el sujeto: {e}"));
            Err(e)
        }
    }
}

/// Rellena el hueco (el alfa) de la foto que manda la página (relleno.rs).
#[tauri::command]
pub async fn ia_rellenar(request: tauri::ipc::Request<'_>) -> Result<tauri::ipc::Response, String> {
    let (rgba, ancho, alto) = foto_de(&request)?;
    tauri::async_runtime::spawn_blocking(move || rellenar(&rgba, ancho, alto))
        .await
        .map_err(|e| format!("La IA falló: {e}"))?
        .map(tauri::ipc::Response::new)
}

fn rellenar(rgba: &[u8], ancho: usize, alto: usize) -> Result<Vec<u8>, String> {
    let mut m = RELLENO.lock().unwrap_or_else(|e| e.into_inner());
    let Some(a) = m.as_mut() else {
        return Err("La IA no está preparada.".into());
    };
    let p = relleno::plan(ancho, alto);
    let t0 = Instant::now();
    let r = a.motor.correr_forma(&[1, 4, p.ph, p.pw], relleno::entrada(rgba, ancho, alto, p));
    a.usado = Instant::now();
    match r {
        Ok((_, planos)) if planos.len() == 3 * p.pw * p.ph => {
            crate::salud::anotar("INFO", "ia", &format!("relleno de {ancho}×{alto} (a {}×{} en {}×{}) en {:.1} s", p.sw, p.sh, p.pw, p.ph, t0.elapsed().as_secs_f32()));
            let rgb = relleno::salida(&planos, p, rgba, ancho, alto);
            Ok(imagen::respuesta(ancho as u32, alto as u32, &rgb))
        }
        Ok((forma, _)) => Err(format!("El modelo contestó {forma:?} y se esperaba [1, 3, {}, {}].", p.ph, p.pw)),
        Err(e) => {
            *m = None;
            crate::salud::anotar("ERROR", "ia", &format!("no se pudo rellenar: {e}"));
            Err(e)
        }
    }
}

#[cfg(all(test, windows))]
mod pruebas {
    use super::*;

    /// De punta a punta en la tarjeta de ESTE equipo (no corre en CI: `cargo test -- --ignored`).
    /// LS_IA_DIR: carpeta de la IA (se baja ahí lo que falte). LS_IA_ENTRADA: opcional, un .f32 con
    /// la entrada [1,3,lado,lado] ya hecha (para comparar con la CPU); la salida va a LS_IA_SALIDA.
    /// LS_IA_MODELO / LS_IA_LADO: probar otro modelo de la carpeta; LS_IA_ESPERA: segundos con el
    /// modelo abierto entre vueltas (para mirar la memoria de la tarjeta).
    #[test]
    #[ignore]
    fn de_punta_a_punta_en_la_tarjeta() {
        let dir = PathBuf::from(std::env::var("LS_IA_DIR").expect("LS_IA_DIR"));
        for p in piezas::de(Que::Sujeto) {
            let t = Instant::now();
            bajar::bajar(&dir, p, None, &mut |_| {}).unwrap();
            eprintln!("{}: listo en {:.1} s", p.nombre, t.elapsed().as_secs_f32());
        }
        let t = Instant::now();
        let mut m = match std::env::var("LS_IA_MODELO") {
            Ok(f) => motor::Motor::abrir_archivo(&dir, &f).unwrap(),
            Err(_) => motor::Motor::abrir(&dir).unwrap(),
        };
        eprintln!("modelo abierto en {:.1} s", t.elapsed().as_secs_f32());
        let lado = std::env::var("LS_IA_LADO").ok().and_then(|v| v.parse().ok()).unwrap_or(piezas::LADO);
        let entrada = match std::env::var("LS_IA_ENTRADA") {
            Ok(f) => std::fs::read(f).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            Err(_) => {
                let (w, h) = (1600usize, 1067usize);
                let mut rgba = vec![255u8; w * h * 4];
                for y in 0..h {
                    for x in 0..w {
                        let i = (y * w + x) * 4;
                        let d = ((x as f32 - 800.0).powi(2) + (y as f32 - 560.0).powi(2)).sqrt() / 300.0;
                        let g = (90.0 + 60.0 * y as f32 / h as f32) as u8;
                        let c = if d < 1.0 { [220, 120, 60] } else { [g, g, g + 10] };
                        rgba[i..i + 3].copy_from_slice(&c);
                    }
                }
                imagen::entrada(&rgba, w, h, lado)
            }
        };
        for vuelta in 0..2 {
            let t = Instant::now();
            let s = m.correr(entrada.clone(), lado).unwrap();
            let (min, max) = s.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
            eprintln!("vuelta {vuelta}: {:.2} s, salida {min:.2}..{max:.2}, logits {}", t.elapsed().as_secs_f32(), imagen::son_logits(&s));
            if let Ok(seg) = std::env::var("LS_IA_ESPERA") {
                eprintln!("esperando {seg} s con el modelo abierto");
                std::thread::sleep(Duration::from_secs(seg.parse().unwrap_or(0)));
            }
            if vuelta == 1 {
                if let Ok(f) = std::env::var("LS_IA_SALIDA") {
                    std::fs::write(f, s.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
                }
            }
        }
    }

    /// Rellenar de punta a punta en ESTE equipo: LS_IA_DIR con el motor ya bajado y el modelo de
    /// rellenar (piezas::ARCHIVO_RELLENO); unas rayas con un hueco cuadrado, al tamaño LS_IA_LADOS.
    #[test]
    #[ignore]
    fn relleno_de_punta_a_punta() {
        let dir = PathBuf::from(std::env::var("LS_IA_DIR").expect("LS_IA_DIR"));
        let t = Instant::now();
        let mut m = motor::Motor::abrir_en_cpu(&dir, piezas::ARCHIVO_RELLENO).unwrap();
        eprintln!("lama abierto en {:.1} s ({})", t.elapsed().as_secs_f32(), m.donde());
        let lados: Vec<usize> = std::env::var("LS_IA_LADOS").unwrap_or("600,1400".into()).split(',').filter_map(|v| v.trim().parse().ok()).collect();
        for &lado in &lados {
            let (w, h) = (lado, lado * 2 / 3);
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let raya: u8 = if (x / 16) % 2 == 0 { 40 } else { 0 };
                    rgba[i] = 80 + raya;
                    rgba[i + 1] = 120;
                    rgba[i + 2] = 50 + raya;
                    let hueco = x > w * 2 / 5 && x < w * 3 / 5 && y > h * 2 / 5 && y < h * 3 / 5;
                    rgba[i + 3] = if hueco { 255 } else { 0 };
                }
            }
            let p = relleno::plan(w, h);
            let t = Instant::now();
            let (_, planos) = m.correr_forma(&[1, 4, p.ph, p.pw], relleno::entrada(&rgba, w, h, p)).unwrap();
            let rgb = relleno::salida(&planos, p, &rgba, w, h);
            let c = (h / 2 * w + w / 2) * 3;
            eprintln!("{w}×{h} ({p:?}): {:.2} s, centro {:?}, esquina {:?}", t.elapsed().as_secs_f32(), &rgb[c..c + 3], &rgb[..3]);
            // fuera del hueco, lo que había
            assert_eq!(&rgb[..3], &rgba[..3]);
        }
    }
}
