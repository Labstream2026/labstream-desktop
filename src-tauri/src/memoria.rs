// ── Memoria (v1.20.0): que la app gaste lo que necesita y devuelva lo que no ──
//
// MEDIDO en el equipo de edición (2026-10-14, app abierta desde el día anterior, 5 pestañas):
// 2.221 MB de memoria privada en 13 procesos. De eso, 1.329 MB eran del PROCESO GRÁFICO de
// WebView2 (gpu-process), que crece con las horas (fotos, miniaturas, el reproductor de revisión,
// dos monitores 4K) y no devuelve nada; cada pestaña viva pesaba 110-180 MB más. Se reinició a
// mano solo el proceso gráfico: WebView2 lo levantó de nuevo, nada se perdió, y la app bajó a
// 1.140 MB. De ahí salen las tres medidas de este módulo:
//
// 1. RECICLAR EL PROCESO GRÁFICO: si pasa de GPU_RECICLAR_MB y nadie está mirando (ventana
//    oculta o minimizada, o sin tocar ratón ni teclado RECICLAR_INACTIVO), se cierra y WebView2
//    crea otro al momento (es un caso previsto: ProcessFailed GPU_PROCESS_EXITED, que salud.rs
//    anota). Como mucho cada RECICLAR_CADA: Chromium pasa a dibujar sin GPU si el proceso gráfico
//    cae varias veces en poco tiempo, y eso sería peor que la memoria.
// 2. NIVEL DE MEMORIA BAJO (ICoreWebView2_19::MemoryUsageTargetLevel): a las pestañas ocultas
//    —y a todas con la ventana oculta o minimizada— se les pide a WebView2 que suelte cachés y
//    memoria que puede regenerar. La pestaña a la vista vuelve a «normal» al instante.
// 3. DESCARTAR PESTAÑAS (lib.rs, `ronda_de_reposo`): una pestaña que lleva DESCARTE_TRAS dormida
//    se cierra de verdad —su proceso sale de la memoria— y se queda en la barra con su título,
//    URL y color; al volver a ella se crea otra vez en la misma URL (1-2 s de carga). Es lo que
//    hace el «Ahorro de memoria» de Chrome. Solo llegan aquí pestañas que ya pasaron el filtro
//    del reposo (sin subidas, descargas ni audio, y nunca un editor de documentos).

// En macOS solo existen los niveles y el descarte; la medición y el reciclaje son de Windows.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::{
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};

use tauri::{AppHandle, Manager, Runtime, Webview};

use crate::salud;

pub const GPU_RECICLAR_MB: u64 = 600;
const RECICLAR_CADA: Duration = Duration::from_secs(30 * 60);
const RECICLAR_INACTIVO: Duration = Duration::from_secs(10 * 60);
pub const DESCARTE_TRAS: Duration = Duration::from_secs(30 * 60);

static ULTIMO_RECICLAJE: Mutex<Option<Instant>> = Mutex::new(None);

/// Memoria de los procesos de WebView2 por tipo, para el diario: (gpu_mb, renderers_mb).
#[derive(Clone, Copy, Default)]
pub struct Reparto {
    pub gpu_mb: u64,
    pub pestanas_mb: u64,
}

// ── Nivel de memoria de una pestaña ──

#[cfg(windows)]
pub fn nivel_bajo<R: Runtime>(wv: &Webview<R>, bajo: bool) {
    let _ = wv.with_webview(move |pw| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        };
        use windows::core::Interface;
        let Ok(core) = pw.controller().CoreWebView2() else { return };
        // ICoreWebView2_19 existe desde el runtime 1.0.2210 (2023): si no está, no pasa nada.
        let Ok(c19) = core.cast::<ICoreWebView2_19>() else { return };
        let _ = c19.SetMemoryUsageTargetLevel(if bajo { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL });
    });
}
#[cfg(not(windows))]
pub fn nivel_bajo<R: Runtime>(_wv: &Webview<R>, _bajo: bool) {}

/// Con la ventana oculta o minimizada, todas las vistas a nivel bajo; si no, solo las ocultas.
/// La llama la ronda de salud cada minuto (desde su hilo: las consultas a la ventana esperan al
/// hilo principal y aquí eso es seguro).
pub fn ajustar_niveles<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_window("main") else { return };
    let nadie_mira = !win.is_visible().unwrap_or(true) || win.is_minimized().unwrap_or(false);
    let activa = {
        let shell = app.state::<crate::ShellState>();
        let s = shell.lock().unwrap();
        s.tabs.get(s.active).map(|t| t.label.clone())
    };
    for wv in win.webviews() {
        let visible = wv.label() == crate::CHROME || Some(wv.label()) == activa.as_deref();
        nivel_bajo(&wv, nadie_mira || !visible);
    }
}

/// La ventana vuelve a verse (bandeja, minimizar, foco): la barra y la pestaña activa recuperan el
/// nivel normal YA, sin esperar a la ronda del minuto. No consulta la ventana: vale en cualquier hilo.
pub fn a_la_vista<R: Runtime>(app: &AppHandle<R>) {
    let activa = {
        let shell = app.state::<crate::ShellState>();
        let s = shell.lock().unwrap();
        s.tabs.get(s.active).map(|t| t.label.clone())
    };
    for label in std::iter::once(crate::CHROME.to_string()).chain(activa) {
        if let Some(wv) = app.get_webview(&label) {
            nivel_bajo(&wv, false);
        }
    }
}

// ── Procesos de WebView2 y reciclaje del proceso gráfico ──

/// (pid, tipo) de todos los procesos del entorno WebView2 de la app. Tipo: el valor de
/// COREWEBVIEW2_PROCESS_KIND (0 navegador, 1 pestaña, 2 utilidad, 3 sandbox, 4 GPU…).
/// No llamar desde el hilo principal: espera su respuesta.
#[cfg(windows)]
fn procesos<R: Runtime>(app: &AppHandle<R>) -> Vec<(u32, i32)> {
    let Some(wv) = app.get_webview(crate::CHROME) else { return Vec::new() };
    let (tx, rx) = mpsc::channel();
    let pedido = wv.with_webview(move |pw| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2Environment8, ICoreWebView2_2, COREWEBVIEW2_PROCESS_KIND};
        use windows::core::Interface;
        let mut v = Vec::new();
        let lista = pw
            .controller()
            .CoreWebView2()
            .and_then(|c| c.cast::<ICoreWebView2_2>())
            .and_then(|c| c.Environment())
            .and_then(|e| e.cast::<ICoreWebView2Environment8>())
            .and_then(|e| e.GetProcessInfos());
        if let Ok(lista) = lista {
            let mut n = 0u32;
            let _ = lista.Count(&mut n);
            for i in 0..n {
                let Ok(info) = lista.GetValueAtIndex(i) else { continue };
                let (mut pid, mut kind) = (0i32, COREWEBVIEW2_PROCESS_KIND::default());
                if info.ProcessId(&mut pid).is_ok() && info.Kind(&mut kind).is_ok() {
                    v.push((pid as u32, kind.0));
                }
            }
        }
        let _ = tx.send(v);
    });
    if pedido.is_err() {
        return Vec::new();
    }
    rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
}

/// Segundos sin tocar ratón ni teclado en todo el equipo.
#[cfg(windows)]
fn inactivo() -> Duration {
    use windows::Win32::{System::SystemInformation::GetTickCount, UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO}};
    unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if !GetLastInputInfo(&mut li).as_bool() {
            return Duration::ZERO;
        }
        Duration::from_millis(GetTickCount().wrapping_sub(li.dwTime) as u64)
    }
}

/// Ronda de memoria (cada minuto, hilo de salud): reparte la memoria por tipo y, si toca,
/// recicla el proceso gráfico.
#[cfg(windows)]
pub fn ronda<R: Runtime>(app: &AppHandle<R>) -> Reparto {
    const GPU: i32 = 4; // COREWEBVIEW2_PROCESS_KIND_GPU
    const PESTANA: i32 = 1; // COREWEBVIEW2_PROCESS_KIND_RENDERER
    let procs = procesos(app);
    let mut r = Reparto::default();
    let mut gpu = None;
    for &(pid, kind) in &procs {
        let mb = salud::memoria_privada_mb(pid);
        match kind {
            GPU => {
                r.gpu_mb += mb;
                gpu = Some((pid, mb));
            }
            PESTANA => r.pestanas_mb += mb,
            _ => {}
        }
    }
    if let Some((pid, mb)) = gpu {
        if mb >= GPU_RECICLAR_MB {
            let nadie_mira = app
                .get_window("main")
                .map(|w| !w.is_visible().unwrap_or(true) || w.is_minimized().unwrap_or(false))
                .unwrap_or(false);
            let toca = ULTIMO_RECICLAJE.lock().unwrap().map(|t| t.elapsed() >= RECICLAR_CADA).unwrap_or(true);
            // Con la ventana a la vista solo si nadie toca el equipo Y no sale sonido: alguien
            // viendo un video de revisión sin tocar nada está «inactivo» pero sí está mirando, y
            // cortarle el proceso gráfico le daría un parpadeo en pleno video.
            let quieto = inactivo() >= RECICLAR_INACTIVO && !crate::tracker::hay_audio_sonando();
            if toca && (nadie_mira || quieto) {
                *ULTIMO_RECICLAJE.lock().unwrap() = Some(Instant::now());
                if terminar(pid) {
                    salud::anotar("INFO", "memoria", &format!("proceso gráfico reciclado: tenía {mb} MB (WebView2 crea otro limpio)"));
                } else {
                    salud::anotar("AVISO", "memoria", &format!("no se pudo reciclar el proceso gráfico ({mb} MB)"));
                }
            }
        }
    }
    r
}
#[cfg(not(windows))]
pub fn ronda<R: Runtime>(_app: &AppHandle<R>) -> Reparto {
    Reparto::default()
}

#[cfg(windows)]
fn terminar(pid: u32) -> bool {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) else { return false };
        let ok = TerminateProcess(h, 0).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}
