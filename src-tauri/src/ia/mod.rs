// ── La IA EN EL EQUIPO: la máscara de sujeto (1.22) y Rellenar (1.23) de Labstream OS ──────────
//
// Munka: «los pcs que usan esta función tienen Nvidia de 16 GB… el modelo se ejecuta en local y se
// conecta y sube el resultado a la app». En el editor de revelado (modo edición de un set):
// - «Sujeto» y «Fondo» los busca esta app: ONNX Runtime con DirectML sobre la tarjeta NVIDIA y
//   BiRefNet en media precisión (piezas.rs): 0,3 s por foto en vez de 2,5–3 s en el navegador.
// - «Rellenar con IA» (1.27): SDXL Inpainting en la tarjeta, en un proceso aparte (generativo.rs):
//   crea lo que se pide con una frase dentro de lo pintado, o quita lo pintado. (La 1.23–1.26
//   rellenaba con LaMa en la CPU; ya no.)
//
// La página (lib/revelado/ia-local en labstream-os) llama a cuatro comandos, permitidos SOLO para el
// origen de la app (capabilities/default.json + el manifiesto de build.rs):
//   ia_estado   ({ que }) → ¿se puede aquí?, ¿está ya bajado?, cuánto falta, qué tarjeta
//   ia_preparar ({ que }) → baja lo que falte (eventos «ia-progreso» { fase, parte } a esa pestaña)
//                           y abre el modelo
//   ia_sujeto   ← RGBA en el cuerpo (binario) + x-ancho / x-alto
//               → [ancho u32][alto u32][un byte por punto] (lib/revelado/ia-local: leerRespuesta)
//   ia_generar  ← RGBA con el hueco en el alfa + x-ancho / x-alto / x-frase (codificada como en
//                 una URL) / x-n / x-pasos / x-semilla / x-fuerza (1..100 %, 1.28) / x-guia (1..15, 1.28)
//               → [n u32][ancho u32][alto u32][semilla u64][RGB de cada propuesta, 3 bytes por punto]
// `que` es "sujeto" (o nada: la página de la 1.22) o "generativo". El resultado vuelve a la página,
// que lo pone en la máscara o en un parche de la foto y lo guarda con su revelado.
//
// Nada se baja ni se carga al arrancar la app: solo cuando alguien lo pide. Lo bajado vive en los
// datos de la app del equipo (`carpeta`), comprobado con su huella (bajar.rs). Cada modelo abierto
// ocupa memoria (el del sujeto, ~7 GB de la tarjeta mientras busca: DirectML guarda lo que pidió):
// se suelta a los SOLTAR_TRAS sin usarlo, que los editores trabajan con Resolve abierto.

mod bajar;
mod generativo;
mod imagen;
mod motor;
mod piezas;
mod tarjeta;

use piezas::Que;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, EventTarget, Manager, Runtime, Webview};

/// Sin usarlo este rato, el modelo del sujeto se suelta de la tarjeta (el generativo se suelta él solo).
const SOLTAR_TRAS: Duration = Duration::from_secs(2 * 60);

struct Abierto {
    motor: motor::Motor,
    usado: Instant,
}

/// El modelo del sujeto abierto (DirectML).
static SUJETO: Mutex<Option<Abierto>> = Mutex::new(None);
/// Una preparación a la vez (dos pestañas que piden a la vez no bajan dos veces).
static PREPARANDO: Mutex<()> = Mutex::new(());
static VIGILANTE: std::sync::Once = std::sync::Once::new();

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
    /// la página pidió una IA que esta app no conoce (p. ej. "relleno", el LaMa de antes)
    desconocida: bool,
    disponible: bool,
    motivo: Option<String>,
    listo: bool,
    faltan: u64,
    modelo: &'static str,
    tarjeta: Option<String>,
}

/// Por qué esta IA no puede correr aquí (None: sí puede). Las dos piden Windows con NVIDIA.
fn motivo(_que: Que, tarjeta: Option<&tarjeta::Tarjeta>) -> Option<String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        Some("La IA del equipo es para la app de Windows.".to_string())
    } else if tarjeta.is_none() {
        Some("Este equipo no tiene tarjeta NVIDIA.".to_string())
    } else {
        None
    }
}

#[tauri::command]
pub fn ia_estado<R: Runtime>(app: AppHandle<R>, que: Option<String>) -> Estado {
    let t = tarjeta::nvidia();
    let Some(que) = Que::de(que.as_deref()) else {
        return Estado { que: "", desconocida: true, disponible: false, motivo: Some("Esta app no tiene esa IA.".into()), listo: false, faltan: 0, modelo: "", tarjeta: t.map(|t| t.nombre) };
    };
    let motivo = motivo(que, t.as_ref());
    let dir = carpeta(&app).ok();
    let falta = dir.as_deref().map(|d| faltan(d, que)).unwrap_or(u64::MAX);
    Estado {
        que: que.nombre(),
        desconocida: false,
        disponible: motivo.is_none() && dir.is_some(),
        motivo,
        listo: falta == 0,
        faltan: if falta == u64::MAX { 0 } else { falta },
        modelo: match que {
            Que::Sujeto => piezas::NOMBRE_MODELO,
            Que::Generativo => piezas::NOMBRE_RELLENO,
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
    let que = Que::de(que.as_deref()).ok_or("Esta app no tiene esa IA.")?;
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
            bajar::bajar_con(
                &dir,
                p,
                sesion,
                &mut |n| {
                    let h = hecho.fetch_add(n, Ordering::Relaxed) + n;
                    // un aviso cada ~1 %
                    if h - ultimo > total / 100 {
                        ultimo = h;
                        avisar(app, label, "bajando", (h as f64 / total as f64).min(1.0));
                    }
                },
                // sacar un zip de miles de archivos (el motor, el modelo) también tarda
                &mut |parte| avisar(app, label, "preparando", parte),
            )
            .map_err(|e| {
                crate::salud::anotar("ERROR", "ia", &e);
                e
            })?;
        }
        crate::salud::anotar("INFO", "ia", &format!("IA de {} bajada al equipo ({} MB)", que.nombre(), total / 1_000_000));
    }
    if que == Que::Generativo {
        generativo::limpiar_lama(&dir);
        avisar(app, label, "preparando", 0.0);
        generativo::arrancar(&dir).map_err(|e| {
            crate::salud::anotar("ERROR", "ia", &format!("no se pudo arrancar el motor generativo: {e}"));
            e
        })?;
        return Ok(());
    }
    let mut m = SUJETO.lock().unwrap_or_else(|e| e.into_inner());
    if m.is_none() {
        avisar(app, label, "preparando", 0.0);
        let t0 = Instant::now();
        let motor = motor::Motor::abrir(&dir).map_err(|e| {
            crate::salud::anotar("ERROR", "ia", &format!("no se pudo abrir el modelo de {}: {e}", que.nombre()));
            e
        })?;
        crate::salud::anotar("INFO", "ia", &format!("modelo de {} abierto en {:.1} s ({})", que.nombre(), t0.elapsed().as_secs_f32(), motor.donde()));
        *m = Some(Abierto { motor, usado: Instant::now() });
        vigilar();
    }
    Ok(())
}

/// Suelta el modelo del sujeto cuando lleva SOLTAR_TRAS sin usarse.
fn vigilar() {
    VIGILANTE.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(15));
            let mut m = SUJETO.lock().unwrap_or_else(|e| e.into_inner());
            if m.as_ref().map(|a| a.usado.elapsed() >= SOLTAR_TRAS).unwrap_or(false) {
                *m = None;
                crate::salud::anotar("INFO", "ia", "modelo de sujeto soltado (sin uso)");
            }
        });
    });
}

/// Al salir de la app: el motor generativo se apaga.
pub fn apagar() {
    generativo::apagar();
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

/// Una cabecera de texto de la página, codificada como en una URL (la frase, con tildes).
fn texto_de(request: &tauri::ipc::Request<'_>, k: &str) -> String {
    let v = request.headers().get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
    percent_encoding::percent_decode_str(v).decode_utf8_lossy().chars().take(600).collect()
}

/// Rellena lo pintado (el alfa) con el motor generativo: `n` propuestas (generativo.rs).
#[tauri::command]
pub async fn ia_generar<R: Runtime>(app: AppHandle<R>, request: tauri::ipc::Request<'_>) -> Result<tauri::ipc::Response, String> {
    let (rgba, ancho, alto) = foto_de(&request)?;
    let numero = |k: &str, d: u64| request.headers().get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(d);
    let pedido = (texto_de(&request, "x-frase"), numero("x-n", 2).clamp(1, 4) as u8, numero("x-pasos", 24).clamp(10, 40) as u8, request.headers().get("x-semilla").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()));
    // 1.28: la fuerza (en % desde la página) y el peso de la frase; sin cabecera, lo de siempre
    let fuerza = numero("x-fuerza", 99).clamp(10, 100) as f32 / 100.0;
    let guia = numero("x-guia", 10).clamp(1, 15) as f32;
    let dir = carpeta(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (frase, n, pasos, semilla) = pedido;
        let (propuestas, semilla) = generativo::rellenar(&dir, &generativo::Pedido { rgba: &rgba, ancho, alto, frase, n, pasos, semilla, fuerza, guia })?;
        // [n u32][ancho u32][alto u32][semilla u64][RGB…]
        let mut out = Vec::with_capacity(20 + propuestas.len() * ancho * alto * 3);
        out.extend_from_slice(&(propuestas.len() as u32).to_le_bytes());
        out.extend_from_slice(&(ancho as u32).to_le_bytes());
        out.extend_from_slice(&(alto as u32).to_le_bytes());
        out.extend_from_slice(&semilla.to_le_bytes());
        for p in &propuestas {
            out.extend_from_slice(p);
        }
        Ok::<Vec<u8>, String>(out)
    })
    .await
    .map_err(|e| format!("La IA falló: {e}"))?
    .map(tauri::ipc::Response::new)
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

}
