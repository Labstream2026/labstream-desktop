// ── Salud de la app (v1.20.0): cuánto gasta de verdad, qué falla y cómo se repone sola ──
//
// POR QUÉ: la app es un exe pequeño (~60 MB) que cuelga de él una docena de procesos de WebView2
// (uno por pestaña, más el GPU y los de utilidad). En el Administrador de tareas el gasto real
// queda repartido entre «Microsoft Edge WebView2» y no se ve cuánto pesa Labstream OS entero; y
// cuando algo fallaba (una pestaña en blanco, un cuelgue) no quedaba rastro. Este módulo:
//
// 1. MIDE cada minuto el ÁRBOL de procesos de la app (el exe + todos sus WebView2): memoria
//    privada, CPU y número de procesos. Lo enseña en la bandeja («Recursos: 2,1 GB · CPU 3 %»)
//    y lo anota en `recursos.csv`, para tener la curva de un día de trabajo real.
//    Columnas del CSV: fecha, procesos, memoria_mb, cpu, pestañas, dormidas, descartadas,
//    gpu_mb (proceso gráfico) y pestanas_mb (suma de los procesos de pestaña).
// 2. ANOTA en `salud.log` lo que falla: pánicos de Rust, procesos de WebView2 caídos o colgados,
//    errores de JavaScript de las páginas, el hilo principal sin responder y los cierres de golpe
//    (si al arrancar sigue el testigo `en-marcha`, la sesión anterior no se cerró bien).
// 3. SE REPONE: una pestaña cuyo proceso murió o se colgó se recarga sola (con freno: más de 3 en
//    10 min y se deja quieta para no entrar en bucle); si muere el proceso principal de WebView2
//    —se caen TODAS las pestañas— la app se reinicia; Windows la vuelve a abrir si se cuelga o
//    se cierra de golpe (RegisterApplicationRestart); y con la memoria por encima del umbral se
//    duermen ya las pestañas ocultas, sin esperar sus 5 minutos.
//
// Todo queda en la carpeta de logs de la app (%LOCALAPPDATA%\co.labstream.os\logs) y se abre
// desde ⋮ ▸ Diagnóstico. No se envía nada a ningún sitio.

use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tauri::{menu::MenuItem, AppHandle, Manager, Runtime};

const LOG_MAX: u64 = 2 * 1024 * 1024; // salud.log rota a salud.1.log al pasar de 2 MB
const CSV_MAX: u64 = 4 * 1024 * 1024;
const MUESTRA_CADA: Duration = Duration::from_secs(60);
/// Memoria privada del árbol a partir de la cual se duermen ya las pestañas ocultas.
pub const MEMORIA_ALTA_MB: u64 = 3 * 1024;
const RECARGAS_MAX: usize = 3; // por pestaña…
const RECARGAS_VENTANA: Duration = Duration::from_secs(10 * 60); // …en esta ventana

static DIR: OnceLock<PathBuf> = OnceLock::new();
static ESCRITURA: Mutex<()> = Mutex::new(());
static ERRORES: AtomicU32 = AtomicU32::new(0);
static RECUPERACIONES: AtomicU32 = AtomicU32::new(0);
static REINICIANDO: AtomicBool = AtomicBool::new(false);
static ULTIMA: Mutex<Option<Muestra>> = Mutex::new(None);
static RECARGAS: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
static VISTOS_JS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

#[derive(Clone, Copy, Default)]
pub struct Muestra {
    pub procesos: u32,
    pub memoria_mb: u64,
    pub cpu: f64,
}

pub struct SaludUi<R: Runtime>(pub MenuItem<R>);

// ── Diario ──

/// Fecha y hora UTC en ISO 8601, sin dependencias (días civiles de Howard Hinnant).
fn ahora_iso() -> String {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (dias, resto) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", resto / 3600, resto % 3600 / 60, resto % 60)
}

fn anexar(nombre: &str, max: u64, linea: &str) {
    let Some(dir) = DIR.get() else { return };
    let _g = ESCRITURA.lock();
    let ruta = dir.join(nombre);
    if fs::metadata(&ruta).map(|m| m.len() > max).unwrap_or(false) {
        let viejo = dir.join(nombre.replacen('.', ".1.", 1));
        let _ = fs::remove_file(&viejo);
        let _ = fs::rename(&ruta, &viejo);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&ruta) {
        let _ = writeln!(f, "{linea}");
    }
}

/// Anota una línea en salud.log. `nivel`: INFO, AVISO o ERROR.
pub fn anotar(nivel: &str, origen: &str, msg: &str) {
    if nivel == "ERROR" {
        ERRORES.fetch_add(1, Ordering::Relaxed);
    }
    let msg = msg.replace(['\r', '\n'], " ");
    anexar("salud.log", LOG_MAX, &format!("{} {nivel:<5} {origen}: {msg}", ahora_iso()));
}

/// Error de JavaScript de una pestaña. El mismo mensaje no se repite más de una vez por minuto.
pub fn error_js(label: &str, tipo: &str, msg: &str, src: &str, ruta: &str) {
    let clave = format!("{tipo}|{msg}|{src}");
    {
        let mut v = VISTOS_JS.get_or_init(Default::default).lock().unwrap();
        if v.get(&clave).map(|t| t.elapsed() < Duration::from_secs(60)).unwrap_or(false) {
            return;
        }
        if v.len() > 500 {
            v.clear();
        }
        v.insert(clave, Instant::now());
    }
    anotar("ERROR", &format!("js {label}"), &format!("{tipo} en {ruta}: {msg} [{src}]"));
}

pub fn carpeta() -> Option<PathBuf> {
    DIR.get().cloned()
}

/// Texto para la bandeja y el menú ⋮.
pub fn resumen() -> String {
    let Some(m) = *ULTIMA.lock().unwrap() else { return "Recursos: midiendo…".into() };
    let mem = if m.memoria_mb >= 1024 { format!("{:.1} GB", m.memoria_mb as f64 / 1024.0).replace('.', ",") } else { format!("{} MB", m.memoria_mb) };
    let mut s = format!("Recursos: {mem} · CPU {:.0} % · {} procesos", m.cpu, m.procesos);
    let (e, r) = (ERRORES.load(Ordering::Relaxed), RECUPERACIONES.load(Ordering::Relaxed));
    if e > 0 || r > 0 {
        s.push_str(&format!(" · {e} errores, {r} recuperadas"));
    }
    s
}

// ── Arranque y salida ──

/// Prepara la carpeta, el registro de pánicos, el testigo de cierre limpio y el reinicio automático.
/// Va lo PRIMERO en setup: así un fallo del propio arranque ya queda anotado.
pub fn preparar<R: Runtime>(app: &AppHandle<R>) {
    let dir = app
        .path()
        .app_log_dir()
        .or_else(|_| app.path().app_local_data_dir().map(|d| d.join("logs")))
        .unwrap_or_else(|_| std::env::temp_dir().join("labstream-os-logs"));
    let _ = fs::create_dir_all(&dir);
    let _ = DIR.set(dir.clone());

    let anterior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let donde = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let que = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        anotar("ERROR", "panico", &format!("{que} ({donde}) hilo={}", std::thread::current().name().unwrap_or("?")));
        anterior(info);
    }));

    let testigo = dir.join("en-marcha");
    if testigo.exists() {
        anotar("ERROR", "app", "la sesión anterior terminó de golpe (cierre forzado, cuelgue, apagón o fallo)");
    }
    let _ = fs::write(&testigo, std::process::id().to_string());
    anotar("INFO", "app", &format!("inicio v{} pid {}", app.package_info().version, std::process::id()));

    #[cfg(windows)]
    unsafe {
        // Si la app se cuelga o se cae, Windows la vuelve a abrir (solo si llevaba ≥60 s en marcha,
        // así que un fallo en el arranque no entra en bucle). Al reiniciar el equipo no: de eso ya
        // se encarga el arranque automático.
        use windows::Win32::System::Recovery::{RegisterApplicationRestart, RESTART_NO_REBOOT};
        if RegisterApplicationRestart(windows::core::PCWSTR::null(), RESTART_NO_REBOOT).is_err() {
            anotar("AVISO", "app", "no se pudo registrar el reinicio automático de Windows");
        }
    }
}

/// Salida limpia: borra el testigo (si no se borra, el próximo arranque anota un cierre de golpe).
pub fn salida_limpia() {
    if REINICIANDO.load(Ordering::Relaxed) {
        anotar("INFO", "app", "reinicio para recuperarse");
    } else {
        anotar("INFO", "app", "salida");
    }
    if let Some(d) = DIR.get() {
        let _ = fs::remove_file(d.join("en-marcha"));
    }
}

/// Justo antes de que el instalador de una actualización cierre la app (en Windows lo hace él, sin
/// pasar por la salida normal): se anota y se quita el testigo, para no contarlo como cierre de golpe.
pub fn antes_de_actualizar(version: &str) {
    anotar("INFO", "actualizacion", &format!("se instala la versión {version}"));
    if let Some(d) = DIR.get() {
        let _ = fs::remove_file(d.join("en-marcha"));
    }
}

/// Hilo de medición y vigilante del hilo principal. `memoria_alta` se llama (como mucho cada 10
/// min) cuando el árbol pasa de MEMORIA_ALTA_MB: lib.rs duerme ahí las pestañas ocultas.
pub fn iniciar<R: Runtime>(app: &AppHandle<R>, item: MenuItem<R>, memoria_alta: impl Fn(&AppHandle<R>) + Send + 'static) {
    app.manage(SaludUi(item));
    let h = app.clone();
    std::thread::Builder::new()
        .name("salud-recursos".into())
        .spawn(move || {
            // El medidor de audio (memoria.rs → tracker::hay_audio_sonando) usa COM en este hilo.
            #[cfg(windows)]
            unsafe {
                use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let mut medidor = Medidor::default();
            let mut ultimo_aviso: Option<Instant> = None;
            let mut pico = 0u64;
            loop {
                std::thread::sleep(MUESTRA_CADA);
                // Primero la memoria (memoria.rs): niveles de las vistas y, si toca, reciclar el
                // proceso gráfico. Luego se mide, para que la muestra ya refleje lo liberado.
                crate::memoria::ajustar_niveles(&h);
                let reparto = crate::memoria::ronda(&h);
                let Some(m) = medidor.medir() else { continue };
                *ULTIMA.lock().unwrap() = Some(m);
                let (pestanas, dormidas, descartadas) = conteo_pestanas(&h);
                anexar(
                    "recursos.csv",
                    CSV_MAX,
                    &format!(
                        "{},{},{},{:.1},{},{},{},{},{}",
                        ahora_iso(), m.procesos, m.memoria_mb, m.cpu, pestanas, dormidas, descartadas, reparto.gpu_mb, reparto.pestanas_mb
                    ),
                );
                if m.memoria_mb > pico + 256 && m.memoria_mb > 1024 {
                    pico = m.memoria_mb;
                    anotar("INFO", "recursos", &format!("nuevo pico de memoria: {} MB en {} procesos ({pestanas} pestañas)", m.memoria_mb, m.procesos));
                }
                if m.memoria_mb >= MEMORIA_ALTA_MB && ultimo_aviso.map(|t| t.elapsed() > Duration::from_secs(600)).unwrap_or(true) {
                    ultimo_aviso = Some(Instant::now());
                    anotar("AVISO", "recursos", &format!("memoria alta ({} MB): se duermen ya las pestañas ocultas", m.memoria_mb));
                    memoria_alta(&h);
                }
                // set_text despacha al hilo principal y espera: aquí, en un hilo propio, es seguro.
                let _ = h.state::<SaludUi<R>>().0.set_text(resumen());
            }
        })
        .ok();

    // ¿Responde el hilo principal? Un cuelgue de la interfaz (la ventana «No responde») no deja
    // ningún otro rastro. Se le manda una tarea vacía cada 20 s; si tarda más de 15 s, se anota
    // cuándo empezó y cuánto duró.
    let h = app.clone();
    std::thread::Builder::new()
        .name("salud-vigilante".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(20));
            let (tx, rx) = std::sync::mpsc::channel();
            let t0 = Instant::now();
            if h.run_on_main_thread(move || {
                let _ = tx.send(());
            })
            .is_err()
            {
                return; // la app está cerrándose
            }
            if rx.recv_timeout(Duration::from_secs(15)).is_err() {
                anotar("ERROR", "app", "el hilo principal no responde (la ventana se congeló)");
                let _ = rx.recv();
                anotar("AVISO", "app", &format!("el hilo principal volvió a responder tras {} s", t0.elapsed().as_secs()));
            }
        })
        .ok();
}

fn conteo_pestanas<R: Runtime>(app: &AppHandle<R>) -> (usize, usize, usize) {
    let shell = app.state::<crate::ShellState>();
    let s = shell.lock().unwrap();
    (s.tabs.len(), s.tabs.iter().filter(|t| t.dormida).count(), s.tabs.iter().filter(|t| t.descartada).count())
}

/// Memoria privada de un proceso, en MB (0 si no se puede abrir).
#[cfg(windows)]
pub fn memoria_privada_mb(pid: u32) -> u64 {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };
    unsafe {
        let Ok(hp) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return 0 };
        let mut mc = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        let ok = GetProcessMemoryInfo(hp, &mut mc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, mc.cb).is_ok();
        let _ = CloseHandle(hp);
        if ok {
            mc.PrivateUsage as u64 / (1024 * 1024)
        } else {
            0
        }
    }
}

// ── Recuperación de pestañas (WebView2) ──

#[cfg_attr(not(windows), allow(dead_code))] // en macOS no hay ProcessFailed: solo lo usa Windows
/// ¿Se puede recargar otra vez esta pestaña? Freno contra bucles: RECARGAS_MAX en RECARGAS_VENTANA.
fn permitir_recarga(label: &str) -> bool {
    let mut m = RECARGAS.get_or_init(Default::default).lock().unwrap();
    let v = m.entry(label.to_string()).or_default();
    v.retain(|t| t.elapsed() < RECARGAS_VENTANA);
    if v.len() >= RECARGAS_MAX {
        return false;
    }
    v.push(Instant::now());
    true
}

/// Se engancha al aviso `ProcessFailed` de WebView2 de una pestaña recién creada.
#[cfg(windows)]
pub fn vigilar_pestana<R: Runtime>(app: &AppHandle<R>, wv: &tauri::Webview<R>) {
    let label = wv.label().to_string();
    let h = app.clone();
    let _ = wv.with_webview(move |pw| unsafe {
        use webview2_com::{Microsoft::Web::WebView2::Win32::*, ProcessFailedEventHandler};
        let Ok(core) = pw.controller().CoreWebView2() else { return };
        let mut token = 0i64;
        let handler = ProcessFailedEventHandler::create(Box::new(move |sender, args| {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            if let Some(a) = args {
                let _ = a.ProcessFailedKind(&mut kind);
            }
            let recargar = |por: &str| {
                if !permitir_recarga(&label) {
                    anotar("ERROR", &format!("pestaña {label}"), &format!("{por} otra vez: ya se recargó {RECARGAS_MAX} veces en 10 min, se deja quieta"));
                    crate::aviso_pestana_rota(&h, &label);
                    return;
                }
                if let Some(c) = &sender {
                    if c.Reload().is_ok() {
                        RECUPERACIONES.fetch_add(1, Ordering::Relaxed);
                        anotar("AVISO", &format!("pestaña {label}"), &format!("{por}: recargada sola"));
                    }
                }
            };
            match kind {
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => {
                    anotar("ERROR", &format!("pestaña {label}"), "su proceso se cerró (pestaña en blanco)");
                    recargar("proceso cerrado");
                }
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => {
                    anotar("ERROR", &format!("pestaña {label}"), "su proceso dejó de responder");
                    recargar("proceso colgado");
                }
                COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => {
                    // Lo avisan TODAS las pestañas a la vez: solo la primera reinicia.
                    if !REINICIANDO.swap(true, Ordering::SeqCst) {
                        anotar("ERROR", "webview2", "se cerró el proceso principal de WebView2: se reinicia la app");
                        RECUPERACIONES.fetch_add(1, Ordering::Relaxed);
                        crate::save_persisted(&h);
                        let hh = h.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(500));
                            salida_limpia();
                            hh.restart();
                        });
                    }
                }
                COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => {
                    anotar("AVISO", &format!("pestaña {label}"), "se cerró el proceso de un marco interno (p. ej. un editor incrustado)");
                }
                COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED => {
                    anotar("AVISO", "webview2", "se reinició el proceso de gráficos (WebView2 lo levanta solo)");
                }
                otro => anotar("AVISO", "webview2", &format!("se cerró un proceso auxiliar (tipo {})", otro.0)),
            }
            Ok(())
        }));
        if core.add_ProcessFailed(&handler, &mut token).is_err() {
            anotar("AVISO", "webview2", "no se pudo vigilar una pestaña");
        }
    });
}
#[cfg(not(windows))]
pub fn vigilar_pestana<R: Runtime>(_app: &AppHandle<R>, _wv: &tauri::Webview<R>) {}

// ── Medición del árbol de procesos ──

#[derive(Default)]
struct Medidor {
    cpu_previa: HashMap<u32, u64>, // pid → tiempo de CPU acumulado (100 ns)
    cuando: Option<Instant>,
}

#[cfg(windows)]
impl Medidor {
    fn medir(&mut self) -> Option<Muestra> {
        use windows::Win32::{
            Foundation::{CloseHandle, FILETIME},
            System::{
                Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX},
                Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
            },
        };
        unsafe {
            // 1. Todos los procesos con su padre; 2. el árbol que cuelga de este exe.
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut padres: Vec<(u32, u32)> = Vec::with_capacity(512);
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut e).is_ok() {
                loop {
                    padres.push((e.th32ProcessID, e.th32ParentProcessID));
                    if Process32NextW(snap, &mut e).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
            let mut arbol = vec![std::process::id()];
            let mut i = 0;
            while i < arbol.len() {
                let p = arbol[i];
                for &(hijo, padre) in &padres {
                    if padre == p && hijo != p && !arbol.contains(&hijo) {
                        arbol.push(hijo);
                    }
                }
                i += 1;
            }

            let mut memoria = 0u64;
            let mut cpu_ahora: HashMap<u32, u64> = HashMap::new();
            for &pid in &arbol {
                let Ok(hp) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { continue };
                let mut mc = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
                if GetProcessMemoryInfo(hp, &mut mc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, mc.cb).is_ok() {
                    memoria += mc.PrivateUsage as u64;
                }
                let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
                if GetProcessTimes(hp, &mut c, &mut x, &mut k, &mut u).is_ok() {
                    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
                    cpu_ahora.insert(pid, t(k) + t(u));
                }
                let _ = CloseHandle(hp);
            }

            // CPU: lo gastado desde la muestra anterior, sobre el tiempo de reloj × núcleos (como
            // la columna CPU del Administrador de tareas). Un proceso que no estaba en la muestra
            // anterior empieza a contar en la siguiente (si no, sumaría toda su vida de golpe).
            let ahora = Instant::now();
            let cpu = match self.cuando {
                Some(antes) => {
                    let pared = ahora.duration_since(antes).as_secs_f64() * 1e7; // en 100 ns
                    let nucleos = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
                    let gastado: u64 = cpu_ahora.iter().filter_map(|(p, t)| self.cpu_previa.get(p).map(|antes| t.saturating_sub(*antes))).sum();
                    (gastado as f64 / (pared * nucleos) * 100.0).clamp(0.0, 100.0)
                }
                None => 0.0,
            };
            self.cpu_previa = cpu_ahora;
            self.cuando = Some(ahora);
            Some(Muestra { procesos: arbol.len() as u32, memoria_mb: memoria / (1024 * 1024), cpu })
        }
    }
}

#[cfg(not(windows))]
impl Medidor {
    fn medir(&mut self) -> Option<Muestra> {
        None // macOS: pendiente (proc_pidinfo); el equipo de edición es Windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fecha_iso_con_formato_fijo() {
        let f = ahora_iso();
        assert_eq!(f.len(), 20, "{f}");
        assert!(f.starts_with("20") && f.ends_with('Z') && &f[10..11] == "T", "{f}");
    }

    #[test]
    fn el_freno_corta_la_cuarta_recarga() {
        let l = "tab-prueba-freno";
        assert!(permitir_recarga(l));
        assert!(permitir_recarga(l));
        assert!(permitir_recarga(l));
        assert!(!permitir_recarga(l), "la cuarta en 10 minutos no se permite");
        assert!(permitir_recarga("otra-pestana"), "el freno es por pestaña");
    }

    #[cfg(windows)]
    #[test]
    fn mide_al_menos_este_proceso() {
        // un binario de prueba casi no reserva memoria: se le da 32 MB tocados para que se note
        let lastre = vec![1u8; 32 * 1024 * 1024];
        let mut m = Medidor::default();
        let a = m.medir().expect("primera muestra");
        assert!(a.procesos >= 1 && a.memoria_mb > 0, "procesos={} memoria={}", a.procesos, a.memoria_mb);
        assert_eq!(a.cpu, 0.0, "sin muestra previa no hay CPU");
        std::thread::sleep(Duration::from_millis(200));
        let b = m.medir().expect("segunda muestra");
        assert!((0.0..=100.0).contains(&b.cpu));
        assert!(lastre.iter().all(|&x| x == 1));
    }
}
