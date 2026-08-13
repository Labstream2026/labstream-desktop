// ── Rastreador de trabajo efectivo (Fase 1) ──
//
// Mide TIEMPO CON EL EQUIPO ACTIVO y en qué app: cada 5 s mira la ventana al frente
// (active-win-pos-rs) y si hubo entrada de mouse/teclado (device_query). Con 3 minutos sin
// entrada deja de contar (ocio profundo); bloquear la pantalla o suspender cae en lo mismo
// porque tampoco hay entrada. Acumula bloques {app, título, segundos, segundos activos} y los
// sube cada 5 min a POST /api/tracker con el token del equipo (ltk_). Sin red, la cola
// persiste en disco y reintenta.
//
// LO QUE NO HACE, a propósito: no registra teclas ni posiciones (solo "hubo entrada"), no
// toma pantallazos, no lista procesos de fondo. El estado es VISIBLE en el menú de la
// bandeja, con pausa de un toque — medición transparente, no spyware.
//
// La vinculación llega por el evento `ls-tracker-token`: la página de Ajustes → Perfil
// (dentro de la app, con la sesión del usuario) genera el token en el servidor y lo emite;
// aquí se guarda en rastreador.json y el sensor arranca. Un 401 del servidor = token
// revocado → se descarta y la bandeja vuelve a «sin vincular».

use std::{
    collections::HashMap,
    sync::Mutex,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{menu::MenuItem, AppHandle, Listener, Manager, Runtime};

const API: &str = "https://os.labstreamsas.com/api/tracker";
const TICK_SEG: u64 = 5;
const OCIO_PROFUNDO_SEG: u64 = 180; // 3 min sin entrada → deja de contar
const LOTE_SEG: u64 = 300; // subir cada 5 min
const MAX_COLA_LOTES: usize = 2000; // ~1 semana sin red; después se pierde lo más viejo
const PAUSA_MANUAL: u64 = u64::MAX;

#[derive(Serialize, Deserialize, Default, Clone)]
struct Config {
    token: Option<String>,
    // Instante unix (seg) hasta el que está pausado; PAUSA_MANUAL = hasta reanudar a mano.
    #[serde(default)]
    pausado_hasta: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Bloque {
    s: u64, // inicio en ms unix
    d: u32, // segundos del bloque
    a: u32, // segundos con entrada real
    app: String,
    t: String, // título de ventana (truncado; puede ir vacío)
}

// Estado compartido sensor ↔ menú. Los items de menú viven aparte (son genéricos en R).
struct Nucleo {
    cfg: Config,
    cola: Vec<Vec<Bloque>>, // lotes pendientes de subir
}

struct NucleoState(Mutex<Nucleo>);
struct MenuUi<R: Runtime> {
    estado: MenuItem<R>,
    toggle: MenuItem<R>,
}
struct MenuState<R: Runtime>(Mutex<MenuUi<R>>);

fn ahora_seg() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
fn ahora_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn ruta_cfg<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("rastreador.json"))
}
fn ruta_cola<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("rastreador-cola.json"))
}

fn cargar_cfg<R: Runtime>(app: &AppHandle<R>) -> Config {
    ruta_cfg(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn guardar_cfg<R: Runtime>(app: &AppHandle<R>, cfg: &Config) {
    let Some(p) = ruta_cfg(app) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string(cfg) {
        let _ = std::fs::write(p, txt);
    }
}
fn cargar_cola<R: Runtime>(app: &AppHandle<R>) -> Vec<Vec<Bloque>> {
    ruta_cola(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn guardar_cola<R: Runtime>(app: &AppHandle<R>, cola: &[Vec<Bloque>]) {
    let Some(p) = ruta_cola(app) else { return };
    if cola.is_empty() {
        let _ = std::fs::remove_file(p);
        return;
    }
    if let Ok(txt) = serde_json::to_string(cola) {
        let _ = std::fs::write(p, txt);
    }
}

// ── Menú de bandeja ──
// Crea los dos items del rastreador (estado informativo + pausar/reanudar) YA con el texto
// correcto: `set_text` de Tauri despacha al hilo principal y BLOQUEA esperándolo, así que
// llamarlo desde `setup()` —que corre en ese mismo hilo, antes de que el bucle de eventos
// empiece a atender— colgaría la app al abrir. Por eso el texto inicial se decide aquí y
// todos los repintados posteriores salen de hilos secundarios (ver `pintar_en_hilo`).
pub fn armar_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<(MenuItem<R>, MenuItem<R>)> {
    let cfg = cargar_cfg(app);
    let estado = MenuItem::with_id(app, "trk-estado", texto_estado(&cfg, "registrando"), false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "trk-toggle", texto_toggle(&cfg), cfg.token.is_some(), None::<&str>)?;
    app.manage(MenuState(Mutex::new(MenuUi { estado: estado.clone(), toggle: toggle.clone() })));
    Ok((estado, toggle))
}

// Maneja los clics del menú del rastreador. Devuelve true si el id era nuestro.
pub fn menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) -> bool {
    match id {
        "trk-toggle" => {
            let nucleo = app.state::<NucleoState>();
            let mut n = nucleo.0.lock().unwrap();
            let pausado = esta_pausado(&n.cfg);
            n.cfg.pausado_hasta = if pausado { None } else { Some(PAUSA_MANUAL) };
            let cfg = n.cfg.clone();
            drop(n);
            guardar_cfg(app, &cfg);
            // El clic del menú llega EN el hilo principal: repintar aquí se bloquearía a sí
            // mismo. Se delega a un hilo suelto, que además responde al instante.
            pintar_en_hilo(app, if esta_pausado(&cfg) { "en pausa" } else { "registrando" }, cfg);
            true
        }
        _ => false,
    }
}

fn pintar_en_hilo<R: Runtime>(app: &AppHandle<R>, estado: &str, cfg: Config) {
    let h = app.clone();
    let estado = estado.to_string();
    thread::spawn(move || pintar_menu(&h, &estado, &cfg));
}

fn esta_pausado(cfg: &Config) -> bool {
    match cfg.pausado_hasta {
        Some(h) => h == PAUSA_MANUAL || h > ahora_seg(),
        None => false,
    }
}

fn texto_estado(cfg: &Config, estado: &str) -> String {
    if cfg.token.is_none() {
        "Rastreador: sin vincular (Ajustes → Perfil)".to_string()
    } else {
        format!("Rastreador: {estado}")
    }
}
fn texto_toggle(cfg: &Config) -> &'static str {
    if esta_pausado(cfg) { "Reanudar el rastreador" } else { "Pausar el rastreador" }
}

// OJO: solo desde hilos secundarios (ver `armar_menu`).
fn pintar_menu<R: Runtime>(app: &AppHandle<R>, estado: &str, cfg: &Config) {
    let Some(menu) = app.try_state::<MenuState<R>>() else { return };
    let ui = menu.0.lock().unwrap();
    let _ = ui.estado.set_text(texto_estado(cfg, estado));
    let _ = ui.toggle.set_text(texto_toggle(cfg));
    let _ = ui.toggle.set_enabled(cfg.token.is_some());
}

// ── Normalización ──
fn normaliza(s: &str, max: usize) -> String {
    let limpio = s.split_whitespace().collect::<Vec<_>>().join(" ");
    limpio.chars().take(max).collect()
}

// ── Envío ──
// Sube la cola entera; deja en la cola lo que no pudo. Un 401 = token revocado → se descarta
// el token (la bandeja pasa a «sin vincular») y la cola se vacía: ya nadie la quiere.
fn enviar<R: Runtime>(app: &AppHandle<R>) {
    let (token, lotes) = {
        let nucleo = app.state::<NucleoState>();
        let n = nucleo.0.lock().unwrap();
        let Some(t) = n.cfg.token.clone() else { return };
        if n.cola.is_empty() {
            return;
        }
        (t, n.cola.clone())
    };
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let version = app.package_info().version.to_string();
    let mut enviados = 0usize;
    let mut revocado = false;
    for lote in &lotes {
        let r = ureq::post(API)
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(20))
            .send_json(json!({
                "device": { "hostname": hostname, "platform": std::env::consts::OS, "version": version },
                "blocks": lote,
            }));
        match r {
            Ok(_) => enviados += 1,
            Err(ureq::Error::Status(401, _)) => {
                revocado = true;
                break;
            }
            Err(_) => break, // sin red o error del servidor: se reintenta en el próximo ciclo
        }
    }
    let nucleo = app.state::<NucleoState>();
    let mut n = nucleo.0.lock().unwrap();
    if revocado {
        n.cfg.token = None;
        n.cola.clear();
        let cfg = n.cfg.clone();
        drop(n);
        guardar_cfg(app, &cfg);
        guardar_cola(app, &[]);
        pintar_menu(app, "sin vincular", &cfg);
        return;
    }
    n.cola.drain(0..enviados.min(n.cola.len()));
    if n.cola.len() > MAX_COLA_LOTES {
        let sobra = n.cola.len() - MAX_COLA_LOTES;
        n.cola.drain(0..sobra); // sin red por semanas: se suelta lo más viejo, no lo nuevo
    }
    let cola = n.cola.clone();
    drop(n);
    guardar_cola(app, &cola);
}

// ── Arranque: listener de vinculación + hilo del sensor ──
pub fn iniciar<R: Runtime>(app: &AppHandle<R>) {
    let cfg = cargar_cfg(app);
    let cola = cargar_cola(app);
    app.manage(NucleoState(Mutex::new(Nucleo { cfg, cola })));
    // El menú ya nació con el texto correcto (armar_menu); repintarlo aquí bloquearía el
    // hilo principal. De aquí en adelante lo repinta el sensor cuando cambia el estado.

    // La página de Ajustes (dentro de la app) entrega el token recién generado.
    let h = app.clone();
    app.listen_any("ls-tracker-token", move |e| {
        #[derive(Deserialize)]
        struct P {
            token: String,
        }
        let Ok(p) = serde_json::from_str::<P>(e.payload()) else { return };
        if !p.token.starts_with("ltk_") {
            return;
        }
        let nucleo = h.state::<NucleoState>();
        let cfg = {
            let mut n = nucleo.0.lock().unwrap();
            n.cfg.token = Some(p.token);
            n.cfg.pausado_hasta = None;
            n.cfg.clone()
        };
        guardar_cfg(&h, &cfg);
        // Sin repintar aquí: el evento puede llegar en el hilo principal. El sensor ve el
        // token en su próximo tic (≤5 s) y actualiza la bandeja solo.
    });

    // El sensor: un hilo con tic de 5 s. Nada de esto toca la interfaz salvo el menú del tray.
    let h = app.clone();
    thread::spawn(move || {
        let device_state = device_query::DeviceState::new();
        let mut ultimo_input = ahora_seg();
        let mut huella_anterior: (i32, i32, usize) = (0, 0, 0);
        let mut acumulado: HashMap<(String, String), Bloque> = HashMap::new();
        let mut ultimo_envio = ahora_seg();
        let mut ultimo_estado = String::new();

        loop {
            thread::sleep(Duration::from_secs(TICK_SEG));
            let ahora = ahora_seg();

            let (token_ok, pausado) = {
                let nucleo = h.state::<NucleoState>();
                let n = nucleo.0.lock().unwrap();
                (n.cfg.token.is_some(), esta_pausado(&n.cfg))
            };

            // ¿Hubo entrada desde el último tic? Solo se compara una huella (posición del
            // mouse + cuántas teclas hay presionadas): nunca se registra QUÉ se tecleó.
            use device_query::DeviceQuery;
            let mouse = device_state.get_mouse();
            let teclas = device_state.get_keys().len();
            let huella = (mouse.coords.0, mouse.coords.1, teclas);
            let hubo_input = huella != huella_anterior || teclas > 0;
            huella_anterior = huella;
            if hubo_input {
                ultimo_input = ahora;
            }
            let ocioso = ahora.saturating_sub(ultimo_input) >= OCIO_PROFUNDO_SEG;

            let estado = if !token_ok {
                "sin vincular"
            } else if pausado {
                "en pausa"
            } else if ocioso {
                "inactivo (no cuenta)"
            } else {
                "registrando"
            };
            if estado != ultimo_estado {
                let cfg = {
                    let nucleo = h.state::<NucleoState>();
                    let n = nucleo.0.lock().unwrap();
                    n.cfg.clone()
                };
                pintar_menu(&h, estado, &cfg);
                ultimo_estado = estado.to_string();
            }

            if token_ok && !pausado && !ocioso {
                if let Ok(v) = active_win_pos_rs::get_active_window() {
                    let app_name = normaliza(&v.app_name, 80);
                    let titulo = normaliza(&v.title, 140);
                    if !app_name.is_empty() {
                        let b = acumulado.entry((app_name.clone(), titulo.clone())).or_insert_with(|| Bloque {
                            s: ahora_ms(),
                            d: 0,
                            a: 0,
                            app: app_name,
                            t: titulo,
                        });
                        b.d += TICK_SEG as u32;
                        if hubo_input {
                            b.a += TICK_SEG as u32;
                        }
                    }
                }
            }

            // Cada 5 min: el acumulado pasa a la cola y se intenta subir todo.
            if ahora.saturating_sub(ultimo_envio) >= LOTE_SEG {
                ultimo_envio = ahora;
                if !acumulado.is_empty() {
                    let lote: Vec<Bloque> = acumulado.drain().map(|(_, b)| b).collect();
                    let nucleo = h.state::<NucleoState>();
                    let cola = {
                        let mut n = nucleo.0.lock().unwrap();
                        n.cola.push(lote);
                        n.cola.clone()
                    };
                    guardar_cola(&h, &cola);
                }
                enviar(&h);
            }
        }
    });
}
