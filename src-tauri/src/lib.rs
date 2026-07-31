// Punto de entrada de la app de escritorio de Labstream OS.
//
// Ya no es un envoltorio de UNA webview: es un SHELL DE PESTAÑAS (multiwebview de Tauri 2).
// La ventana "main" contiene:
//   - un webview "chrome" (dist/index.html, local): la barra de pestañas + zoom, con acceso IPC.
//   - N webviews "tab-*" (remotos): cada pestaña carga el servidor (os.labstreamsas.com).
//
// Con esto los enlaces internos con target=_blank (documentos, entregables, exportaciones,
// OnlyOffice…) — que antes MORÍAN en silencio — abren una pestaña dentro de la app, igual que
// en el navegador. Cmd/Ctrl+clic también abre pestaña. Los enlaces a OTRO origen siguen yendo
// al navegador del sistema.
//
// Comportamiento de cliente nativo que se conserva del envoltorio anterior:
//   - plugin `opener`: enlaces externos al navegador del sistema.
//   - plugin `notification`: la web (window.__TAURI__) muestra avisos nativos.
//   - plugin `autostart`: la app arranca al iniciar sesión.
//   - plugin `updater`: busca versión nueva en los Releases de GitHub y se actualiza sola.
//   - bandeja (tray) + "cerrar = ocultar": sigue corriendo y notificando en segundo plano.
// Y lo nuevo:
//   - ATRÁS/ADELANTE: botones ← → en la barra, ⌘[ / ⌘] (Alt+←/→ en Windows) y menú
//     Historial (macOS). Antes una página sin menú propio era un callejón sin salida.
//   - «Reabrir pestaña cerrada» (⌘/Ctrl+Shift+T), como en el navegador.
//   - Globo de NO-LEÍDOS en el icono (evento `ls-badge` desde la web app): número en el
//     Dock (macOS), punto rojo en la barra de tareas (Windows) y tooltip de la bandeja.
//   - ZOOM de interfaz (Cmd/Ctrl +/−/0 y Ctrl+rueda) con persistencia entre sesiones.
//   - Sesión restaurada: al reabrir vuelve con las mismas pestañas y tamaño de ventana.
//   - single-instance (solo release): abrir la app de nuevo enfoca la ventana existente.
//   - macOS: clic en el Dock reabre la ventana oculta (RunEvent::Reopen) y menú nativo con
//     atajos de pestañas/zoom (los del teclado los consume el menú; en Windows los maneja JS).
//
// La comunicación shell ↔ Rust va por EVENTOS (`__TAURI__.event`), permitidos por
// `core:default` también para el origen remoto (capabilities/default.json). Así no hace falta
// declarar comandos propios en el ACL para páginas remotas.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::{DownloadEvent, PageLoadEvent, Webview, WebviewBuilder},
    window::{Window, WindowBuilder},
    AppHandle, Emitter, EventTarget, Listener, LogicalPosition, LogicalSize, Manager, Runtime,
    Url, WebviewUrl, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

const SERVER_URL: &str = "https://os.labstreamsas.com";
const TAB_BAR_H: f64 = 40.0; // alto lógico de la barra de pestañas (chrome)
const CHROME: &str = "chrome";
// Hueco que ocupan los botones rojo/amarillo/verde en macOS: la barra deja libre ese
// espacio a la izquierda (lo aplica el CSS de la barra con la bandera `mac` del payload).

// Escalones de zoom (como un navegador). 1.0 = 100%.
const ZOOM_STEPS: &[f64] = &[0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5];
const ZOOM_100: usize = 5; // índice de 1.0 en ZOOM_STEPS

// ── Estado del shell ──

struct TabRec {
    label: String,
    title: String,
    url: String,
    loading: bool, // la barra pinta un giro en el icono mientras carga
    // Color elegido a mano desde el menú de la pestaña (None = sin teñir). Aquí solo vive el
    // NOMBRE; el tono exacto y cómo se pinta son cosa del CSS de la barra.
    color: Option<String>,
}

// Paleta de colores de pestaña, la misma idea que los grupos de Chrome: sirve para agrupar de
// un vistazo («todo lo de este cliente en verde»). El id viaja en el payload de `ls-tabs`.
const TAB_COLORS: &[(&str, &str)] = &[
    ("gris", "Gris"),
    ("azul", "Azul"),
    ("rojo", "Rojo"),
    ("amarillo", "Amarillo"),
    ("verde", "Verde"),
    ("rosa", "Rosa"),
    ("morado", "Morado"),
    ("cian", "Cian"),
    ("naranja", "Naranja"),
];

struct Shell {
    tabs: Vec<TabRec>,
    active: usize,
    zoom_idx: usize,
    next_id: u64,
    win: Option<WinRect>,
    autostart: Option<bool>, // elección del usuario en el menú ⋮ (None = por defecto, activado)
    closed: Vec<String>,     // URLs de pestañas cerradas (para «Reabrir pestaña cerrada», ⌘⇧T)
    menu_tab: Option<String>, // pestaña sobre la que se abrió el menú de clic derecho
    // Último estado maximizado conocido. Solo sirve para no repintar la barra en CADA
    // evento de redimensión: únicamente cuando el icono ▢/❐ tiene que cambiar.
    maximized: bool,
    // Tema REAL de la app (la clase `dark` de <html>, reportada por el script inyectado).
    // Es global y no por pestaña: todas las pestañas del origen comparten el mismo modo
    // (localStorage). None = ninguna pestaña ha reportado aún → la barra cae al del sistema.
    dark: Option<bool>,
}

impl Default for Shell {
    fn default() -> Self {
        Self { tabs: Vec::new(), active: 0, zoom_idx: ZOOM_100, next_id: 1, win: None, autostart: None, closed: Vec::new(), menu_tab: None, maximized: false, dark: None }
    }
}

type ShellState = Mutex<Shell>;

#[derive(Clone, Copy, Serialize, Deserialize)]
struct WinRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// Lo que se guarda en disco entre sesiones (zoom, ventana y pestañas abiertas).
#[derive(Serialize, Deserialize, Default)]
struct Persisted {
    #[serde(default)]
    zoom_idx: Option<usize>,
    #[serde(default)]
    win: Option<WinRect>,
    #[serde(default)]
    tabs: Vec<String>,
    // Color de cada pestaña, en el mismo orden que `tabs` ("" = sin teñir). Lista aparte para
    // que un estado.json viejo (sin colores) siga cargando tal cual.
    #[serde(default)]
    colors: Vec<String>,
    #[serde(default)]
    active: usize,
    #[serde(default)]
    autostart: Option<bool>,
    // Último tema conocido de la app: la barra abre ya del color correcto en vez de
    // arrancar con el del sistema y dar un fogonazo al cargar la primera pestaña.
    #[serde(default)]
    dark: Option<bool>,
}

fn state_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("estado.json"))
}

fn load_persisted<R: Runtime>(app: &AppHandle<R>) -> Persisted {
    state_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_persisted<R: Runtime>(app: &AppHandle<R>) {
    let Some(path) = state_path(app) else { return };
    let data = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        Persisted {
            zoom_idx: Some(s.zoom_idx),
            win: s.win,
            tabs: s.tabs.iter().map(|t| t.url.clone()).collect(),
            colors: s.tabs.iter().map(|t| t.color.clone().unwrap_or_default()).collect(),
            active: s.active,
            autostart: s.autostart,
            dark: s.dark,
        }
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string_pretty(&data) {
        let _ = std::fs::write(path, txt);
    }
}

// ── Script inyectado en CADA página de las pestañas (origen remoto) ──
// - Enlaces a OTRO origen → navegador del sistema (opener), como antes.
// - target=_blank internos, Cmd/Ctrl+clic y window.open internos → pestaña nueva en la app.
// - Reporta título/URL a la barra (observer del <title> + pushState del SPA).
// - Atajos de teclado (en macOS los consume antes el menú nativo; esto cubre Windows y extras)
//   y Ctrl+rueda para zoom.
// SOLO debe gobernar el documento principal de NUESTRO origen; ver la guarda de abajo.
const INIT_JS_TPL: &str = r#"
(function () {
  var ORIGIN = "__ORIGIN__";

  // ── GUARDA DE MARCO Y ORIGEN ──
  // Antes aquí solo estaba `if (!T || !T.event) return;`, confiando en que __TAURI__ no
  // existiría fuera del origen permitido. Eso es cierto en macOS y FALSO en Windows:
  //   · wry inyecta los scripts de inicio en TODOS los marcos e IGNORA for_main_frame_only
  //     (wry-0.55.1/src/webview2/mod.rs:492-495, documentado en su lib.rs:990), mientras que
  //     WKWebView sí la respeta (wry-0.55.1/src/wkwebview/mod.rs:643-645);
  //   · y __TAURI__ viaja con ellos (withGlobalTauri, tauri/src/manager/webview.rs:159-165).
  // Consecuencia real: dentro del iframe del editor de OnlyOffice (docs.labstreamsas.com)
  // este script SÍ corría en Windows, con ORIGIN apuntando todavía a la app. Como allí
  // `location.href` es el del Document Server, abs() resolvía CADA enlace del editor contra
  // docs.* → isExt() cierto → preventDefault() + mandarlo al navegador del sistema (que no
  // tiene la sesión: «no se puede acceder»), y window.open devolvía null. Por eso editar y
  // descargar fallaban solo en la app de Windows, y no en Mac ni en el navegador.
  // En macOS estas dos líneas son un no-op: allí el script nunca ha corrido en subframes.
  try { if (window.top !== window.self) return; } catch (e) { return; }
  if (location.origin !== ORIGIN) return; // p. ej. el login del SSO: no intervenir

  if (window.__lsShell) return;
  window.__lsShell = true;
  var T = window.__TAURI__;
  if (!T || !T.event) return;
  var LABEL = "__LABEL__";

  function abs(h) { try { return new URL(h, location.href); } catch (e) { return null; } }
  function isHttp(u) { return !!u && (u.protocol === 'http:' || u.protocol === 'https:'); }
  function isApp(u) { return isHttp(u) && u.origin === ORIGIN; }
  function isExt(u) { return isHttp(u) && u.origin !== ORIGIN; }
  function emit(name, payload) { try { T.event.emit(name, payload); } catch (e) {} }
  function openExternal(url) {
    try { T.core.invoke('plugin:opener|open_url', { url: url, with: null }); }
    catch (e) { try { T.opener.openUrl(url); } catch (_) {} }
  }
  function newTab(url) { emit('ls-new-tab', { url: url }); }

  // Título, URL y TEMA → barra de pestañas (y para restaurar la sesión al reabrir).
  // El tema viaja aquí porque la web decide su modo con su propio botón (next-themes,
  // clase `dark` en <html>) y NO sigue al sistema: la barra tiene que copiarle a ella.
  function report() {
    emit('ls-title', {
      label: LABEL,
      title: document.title || 'Labstream OS',
      url: location.href,
      dark: document.documentElement.classList.contains('dark')
    });
  }
  function watchTitle() {
    var el = document.querySelector('title');
    if (el && window.MutationObserver) {
      new MutationObserver(report).observe(el, { childList: true, characterData: true, subtree: true });
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { watchTitle(); report(); });
  } else { watchTitle(); }
  addEventListener('load', report);
  report();
  ['pushState', 'replaceState'].forEach(function (k) {
    var o = history[k];
    if (!o) return;
    history[k] = function () { var r = o.apply(this, arguments); setTimeout(report, 0); return r; };
  });
  addEventListener('popstate', function () { setTimeout(report, 0); });
  // Pulsar la luna cambia la clase de <html> sin navegar: se observa para que la barra
  // se entere al instante.
  if (window.MutationObserver) {
    new MutationObserver(report).observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  }

  // Clics en enlaces.
  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0) return;
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var u = abs(a.getAttribute('href'));
    if (!u) return;
    if (isExt(u)) { e.preventDefault(); openExternal(u.href); return; }
    var mod = e.metaKey || e.ctrlKey;
    if (isApp(u) && (mod || a.target === '_blank')) { e.preventDefault(); newTab(u.href); }
  }, true);

  // Clic con la rueda (botón central) en un enlace interno → pestaña nueva, como en el navegador.
  document.addEventListener('auxclick', function (e) {
    if (e.button !== 1) return;
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var u = abs(a.getAttribute('href'));
    if (isExt(u)) { e.preventDefault(); openExternal(u.href); return; }
    if (isApp(u)) { e.preventDefault(); newTab(u.href); }
  }, true);

  var _open = window.open;
  window.open = function (url, name, feats) {
    var u = url ? abs(url) : null;
    if (isExt(u)) { openExternal(u.href); return null; }
    if (isApp(u)) { newTab(u.href); return null; }
    return _open ? _open.call(window, url, name, feats) : null;
  };

  // Atajos de teclado.
  addEventListener('keydown', function (e) {
    // AltGr enciende ctrlKey en Windows. Sin este filtro, escribir [ o ] con teclado
    // español —que se hacen con AltGr— entraba por aquí y disparaba history.back() /
    // history.forward() en vez de escribir el carácter: en el editor, en el chat, en una
    // nota y en cualquier campo de la app. En macOS no aplica (Option no toca ctrlKey).
    if (e.altKey) return;
    var mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    var k = e.key;
    if (k === 't' || k === 'T') {
      e.preventDefault();
      if (e.shiftKey) emit('ls-reopen', {}); else newTab(null);
    }
    else if (k === 'w' || k === 'W') { e.preventDefault(); emit('ls-close-tab', { label: LABEL }); }
    else if ((k === 'r' || k === 'R') && !e.shiftKey) { e.preventDefault(); location.reload(); }
    else if (k === '[') { e.preventDefault(); history.back(); }
    else if (k === ']') { e.preventDefault(); history.forward(); }
    else if (k === '=' || k === '+') { e.preventDefault(); emit('ls-zoom', { dir: 1 }); }
    else if (k === '-') { e.preventDefault(); emit('ls-zoom', { dir: -1 }); }
    else if (k === '0') { e.preventDefault(); emit('ls-zoom', { dir: 0 }); }
    else if (k === 'Tab') { e.preventDefault(); emit('ls-cycle', { dir: e.shiftKey ? -1 : 1 }); }
    else if (k >= '1' && k <= '9') { e.preventDefault(); emit('ls-activate-n', { n: +k }); }
  }, true);

  // Alt+←/→ = atrás/adelante (convención de Windows/Linux; en Mac es ⌘[ / ⌘] y Alt+flecha
  // se respeta para saltar palabras al escribir). Fuera de campos de texto solamente.
  var MAC = /Mac/i.test(navigator.platform || '');
  function editable(el) {
    return !!el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName || ''));
  }
  addEventListener('keydown', function (e) {
    if (MAC || !e.altKey || e.metaKey || e.ctrlKey || e.shiftKey || editable(e.target)) return;
    if (e.key === 'ArrowLeft') { e.preventDefault(); history.back(); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); history.forward(); }
  }, true);

  // Ctrl+rueda = zoom (convención de navegador; en Mac también con Ctrl).
  addEventListener('wheel', function (e) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    emit('ls-zoom', { dir: e.deltaY < 0 ? 1 : -1 });
  }, { passive: false, capture: true });
})();
"#;

// ── Utilidades del shell ──

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<Window<R>> {
    app.get_window("main")
}

fn get_webview<R: Runtime>(app: &AppHandle<R>, label: &str) -> Option<Webview<R>> {
    main_window(app).and_then(|w| w.webviews().into_iter().find(|v| v.label() == label))
}

// Geometría: barra arriba (alto fijo), contenido debajo ocupando el resto.
fn layout_all<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = main_window(app) else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let size = size.to_logical::<f64>(scale);
    let content_h = (size.height - TAB_BAR_H).max(0.0);
    for wv in win.webviews() {
        if wv.label() == CHROME {
            let _ = wv.set_position(LogicalPosition::new(0.0, 0.0));
            let _ = wv.set_size(LogicalSize::new(size.width, TAB_BAR_H));
        } else {
            let _ = wv.set_position(LogicalPosition::new(0.0, TAB_BAR_H));
            let _ = wv.set_size(LogicalSize::new(size.width, content_h));
        }
    }
}

fn current_zoom<R: Runtime>(app: &AppHandle<R>) -> f64 {
    let shell = app.state::<ShellState>();
    let s = shell.lock().unwrap();
    ZOOM_STEPS[s.zoom_idx.min(ZOOM_STEPS.len() - 1)]
}

// Manda el estado de pestañas + zoom + versión a la barra (webview "chrome").
fn broadcast<R: Runtime>(app: &AppHandle<R>) {
    // Se consulta ANTES de tomar el lock del shell: así no se anidan dos cerrojos.
    let maximized = main_window(app).and_then(|w| w.is_maximized().ok()).unwrap_or(false);
    let payload = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        let tabs: Vec<_> = s
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| json!({ "label": t.label, "title": t.title, "url": t.url, "loading": t.loading, "active": i == s.active, "color": t.color }))
            .collect();
        json!({
            "tabs": tabs,
            "zoom": (ZOOM_STEPS[s.zoom_idx.min(ZOOM_STEPS.len()-1)] * 100.0).round() as u32,
            "version": app.package_info().version.to_string(),
            // En macOS la barra ocupa la fila del título (ventana con título en overlay) y
            // debe dejar libre el hueco del semáforo, que lo pinta el sistema.
            "mac": cfg!(target_os = "macos"),
            // En Windows la ventana va SIN decoración: la barra es la fila del título, así
            // que dibuja ella misma los botones minimizar/maximizar/cerrar (no hay nativos).
            "win": cfg!(target_os = "windows"),
            "maximized": maximized,
            // Tema real de la app (null hasta el primer reporte: la barra usa el del sistema).
            "dark": s.dark,
        })
    };
    let _ = app.emit_to(EventTarget::webview(CHROME), "ls-tabs", payload);
}

// Marca una pestaña como «cargando» y refresca la barra (giro en el icono).
fn set_loading<R: Runtime>(app: &AppHandle<R>, label: &str, loading: bool) {
    {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        let Some(t) = s.tabs.iter_mut().find(|t| t.label == label) else { return };
        if t.loading == loading {
            return; // sin cambios: no repintar la barra
        }
        t.loading = loading;
    }
    broadcast(app);
}

// Muestra la pestaña `label` y esconde las demás. Devuelve el foco al contenido.
fn activate_tab<R: Runtime>(app: &AppHandle<R>, label: &str) {
    {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        if let Some(i) = s.tabs.iter().position(|t| t.label == label) {
            s.active = i;
        } else {
            return;
        }
    }
    let Some(win) = main_window(app) else { return };
    for wv in win.webviews() {
        if wv.label() == CHROME {
            continue;
        }
        if wv.label() == label {
            let _ = wv.show();
            let _ = wv.set_focus();
        } else {
            let _ = wv.hide();
        }
    }
    broadcast(app);
    save_persisted(app);
}

// Crea una pestaña nueva cargando `url` (o el servidor si viene vacío).
fn create_tab<R: Runtime>(app: &AppHandle<R>, url: Option<String>, activate: bool) {
    let target = url.unwrap_or_else(|| SERVER_URL.to_string());
    // Solo el origen de la app puede abrirse en pestaña; cualquier otra cosa va al navegador.
    let Ok(parsed) = Url::parse(&target) else { return };
    let Ok(server) = Url::parse(SERVER_URL) else { return };
    if parsed.origin() != server.origin() {
        return;
    }

    let label = {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        let label = format!("tab-{}", s.next_id);
        s.next_id += 1;
        s.tabs.push(TabRec { label: label.clone(), title: "Labstream OS".into(), url: target.clone(), loading: true, color: None });
        label
    };

    let Some(win) = main_window(app) else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    let size = win.inner_size().map(|s| s.to_logical::<f64>(scale)).unwrap_or(LogicalSize::new(1400.0, 900.0));

    let init = INIT_JS_TPL.replace("__LABEL__", &label).replace("__ORIGIN__", SERVER_URL);
    let app_zoom = app.clone();
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed))
        .initialization_script(&init)
        // Deja pasar las descargas (entregables, exportaciones) Y avisa al terminar.
        // El aviso no es un adorno: en WebView2 la interfaz de descargas queda SIEMPRE
        // apagada. wry trae un manejador de descargas por defecto (wry-0.55.1/src/lib.rs:830),
        // asi que DownloadStarting se registra siempre y siempre responde SetHandled(true)
        // (wry-0.55.1/src/webview2/mod.rs:858-861), lo que le dice a WebView2 que del aviso
        // se encarga la app: ni barra de progreso, ni globo de «descarga terminada». El
        // archivo bajaba bien a la carpeta de Descargas, pero NADA lo decia y parecia roto.
        // Quitar este manejador NO devuelve la interfaz nativa: el de wry sigue puesto.
        .on_download(|webview, event| {
            if let DownloadEvent::Finished { path, success, .. } = &event {
                let cuerpo = if !*success {
                    "No se pudo completar la descarga.".to_string()
                } else if let Some(p) = path {
                    // En macOS `path` llega siempre vacio (limite de WKDownload:
                    // wry-0.55.1/src/wkwebview/download.rs:98), asi que en la practica
                    // este brazo con nombre de archivo es el de Windows.
                    match p.file_name() {
                        Some(n) => format!("Descargado: {}", n.to_string_lossy()),
                        None => "Descarga terminada.".to_string(),
                    }
                } else {
                    "Descarga terminada. Esta en tu carpeta de Descargas.".to_string()
                };
                let _ = webview
                    .app_handle()
                    .notification()
                    .builder()
                    .title("Labstream OS")
                    .body(cuerpo)
                    .show();
            }
            true
        })
        // El zoom guardado se re-aplica en cada carga (el factor no siempre sobrevive a la navegación).
        .on_page_load(move |wv, payload| {
            let cargando = matches!(payload.event(), PageLoadEvent::Started);
            if !cargando {
                let _ = wv.set_zoom(current_zoom(&app_zoom));
            }
            set_loading(&app_zoom, wv.label(), cargando);
        });

    let created = win.add_child(
        builder,
        LogicalPosition::new(0.0, TAB_BAR_H),
        LogicalSize::new(size.width, (size.height - TAB_BAR_H).max(0.0)),
    );

    match created {
        Ok(_) => {
            if activate {
                activate_tab(app, &label);
            } else {
                broadcast(app);
                save_persisted(app);
            }
        }
        Err(_) => {
            // Revertir el registro si el webview no se pudo crear.
            let shell = app.state::<ShellState>();
            let mut s = shell.lock().unwrap();
            s.tabs.retain(|t| t.label != label);
        }
    }
}

// Cierra una pestaña; si era la activa, activa la vecina. Nunca deja cero pestañas.
fn close_tab<R: Runtime>(app: &AppHandle<R>, label: &str) {
    let next_active: Option<String> = {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        let Some(i) = s.tabs.iter().position(|t| t.label == label) else { return };
        let was_active = i == s.active;
        // Recuerda la URL para «Reabrir pestaña cerrada» (tope de 20, las más recientes).
        let url = s.tabs[i].url.clone();
        s.closed.push(url);
        if s.closed.len() > 20 {
            s.closed.remove(0);
        }
        s.tabs.remove(i);
        if s.tabs.is_empty() {
            s.active = 0;
            None
        } else {
            if s.active >= s.tabs.len() {
                s.active = s.tabs.len() - 1;
            } else if !was_active && i < s.active {
                s.active -= 1;
            }
            was_active.then(|| s.tabs[s.active].label.clone())
        }
    };

    if let Some(wv) = get_webview(app, label) {
        let _ = wv.close();
    }

    let is_empty = {
        let shell = app.state::<ShellState>();
        let n = shell.lock().unwrap().tabs.len();
        n == 0
    };
    if is_empty {
        create_tab(app, None, true);
    } else if let Some(next) = next_active {
        activate_tab(app, &next);
    } else {
        broadcast(app);
        save_persisted(app);
    }
}

// Aplica un paso de zoom (+1 / −1 / 0 = restablecer) a TODAS las pestañas y lo persiste.
fn zoom_step<R: Runtime>(app: &AppHandle<R>, dir: i32) {
    {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        s.zoom_idx = match dir {
            0 => ZOOM_100,
            d if d > 0 => (s.zoom_idx + 1).min(ZOOM_STEPS.len() - 1),
            _ => s.zoom_idx.saturating_sub(1),
        };
    }
    let z = current_zoom(app);
    if let Some(win) = main_window(app) {
        for wv in win.webviews() {
            if wv.label() != CHROME {
                let _ = wv.set_zoom(z);
            }
        }
    }
    broadcast(app);
    save_persisted(app);
}

fn active_label<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let shell = app.state::<ShellState>();
    let s = shell.lock().unwrap();
    s.tabs.get(s.active).map(|t| t.label.clone())
}

fn reload_tab<R: Runtime>(app: &AppHandle<R>, label: Option<String>) {
    let label = label.or_else(|| active_label(app));
    if let Some(l) = label {
        if let Some(wv) = get_webview(app, &l) {
            let _ = wv.eval("location.reload()");
        }
    }
}

// Rota a la pestaña siguiente/anterior (Ctrl+Tab / Ctrl+Shift+Tab).
fn cycle_tab<R: Runtime>(app: &AppHandle<R>, dir: i32) {
    let label = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        if s.tabs.is_empty() {
            return;
        }
        let n = s.tabs.len() as i32;
        let i = ((s.active as i32 + dir) % n + n) % n;
        s.tabs[i as usize].label.clone()
    };
    activate_tab(app, &label);
}

// Activa la pestaña n (1-9; 9 = última, como en los navegadores).
fn activate_nth<R: Runtime>(app: &AppHandle<R>, n: usize) {
    let label = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        if s.tabs.is_empty() {
            return;
        }
        let i = if n >= 9 || n > s.tabs.len() { s.tabs.len() - 1 } else { n - 1 };
        s.tabs[i].label.clone()
    };
    activate_tab(app, &label);
}

fn active_url<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let shell = app.state::<ShellState>();
    let s = shell.lock().unwrap();
    s.tabs.get(s.active).map(|t| t.url.clone())
}

// Atrás/adelante en el HISTORIAL de la pestaña activa. Antes no existía por ningún lado y
// una página sin menú propio (p. ej. el portal de revisión) era un callejón sin salida.
fn nav_tab<R: Runtime>(app: &AppHandle<R>, dir: i32) {
    if let Some(l) = active_label(app) {
        if let Some(wv) = get_webview(app, &l) {
            let _ = wv.eval(if dir < 0 { "history.back()" } else { "history.forward()" });
        }
    }
}

// «Reabrir pestaña cerrada» (⌘/Ctrl+Shift+T): la más reciente primero, como en el navegador.
fn reopen_tab<R: Runtime>(app: &AppHandle<R>) {
    let url = {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        s.closed.pop()
    };
    if let Some(u) = url {
        create_tab(app, Some(u), true);
    }
}

// Punto rojo para el icono de la barra de tareas de Windows (dibujado a mano: sin assets).
#[cfg(target_os = "windows")]
fn red_dot() -> tauri::image::Image<'static> {
    const S: usize = 16;
    let mut px = vec![0u8; S * S * 4];
    let c = (S as f64 - 1.0) / 2.0;
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f64 - c, y as f64 - c);
            if (dx * dx + dy * dy).sqrt() <= c {
                let i = (y * S + x) * 4;
                px[i] = 225;
                px[i + 1] = 29;
                px[i + 2] = 72;
                px[i + 3] = 255;
            }
        }
    }
    tauri::image::Image::new_owned(px, S as u32, S as u32)
}

// Globo de NO-LEÍDOS en el icono, alimentado por la web app (evento `ls-badge {count}`):
// macOS = número en el icono del Dock; Windows = punto rojo sobre el icono de la barra de
// tareas; y en ambos, el tooltip de la bandeja dice cuántos hay.
fn set_badge<R: Runtime>(app: &AppHandle<R>, count: i64) {
    if let Some(win) = main_window(app) {
        #[cfg(target_os = "macos")]
        let _ = win.set_badge_count(if count > 0 { Some(count) } else { None });
        #[cfg(target_os = "windows")]
        let _ = win.set_overlay_icon(if count > 0 { Some(red_dot()) } else { None });
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = &win;
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let tip = if count > 0 { format!("Labstream OS — {count} sin leer") } else { "Labstream OS".to_string() };
        let _ = tray.set_tooltip(Some(tip));
    }
}

// Menú de OPCIONES de la app (botón ⋮ / clic derecho en la barra): un popup NATIVO del sistema,
// igual en Windows y macOS. Se construye fresco en cada apertura para que el check de
// "Iniciar con el sistema" y la versión estén siempre al día. Solo opciones del SHELL — la web
// app no se toca.
fn show_app_menu<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = main_window(app) else { return };
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let version = format!("Versión {}", app.package_info().version);
    let build = || -> tauri::Result<Menu<R>> {
        Ok(Menu::with_items(
            app,
            &[
                &MenuItem::with_id(app, "nav-back", "Atrás", true, Some("CmdOrCtrl+["))?,
                &MenuItem::with_id(app, "nav-forward", "Adelante", true, Some("CmdOrCtrl+]"))?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "new-tab", "Nueva pestaña", true, Some("CmdOrCtrl+T"))?,
                &MenuItem::with_id(app, "close-tab", "Cerrar pestaña", true, Some("CmdOrCtrl+W"))?,
                &MenuItem::with_id(app, "reopen-tab", "Reabrir pestaña cerrada", true, Some("CmdOrCtrl+Shift+T"))?,
                &MenuItem::with_id(app, "reload", "Recargar", true, Some("CmdOrCtrl+R"))?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "zoom-in", "Acercar", true, Some("CmdOrCtrl+="))?,
                &MenuItem::with_id(app, "zoom-out", "Alejar", true, Some("CmdOrCtrl+-"))?,
                &MenuItem::with_id(app, "zoom-0", "Tamaño real", true, Some("CmdOrCtrl+0"))?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "open-browser", "Abrir esta página en el navegador", true, None::<&str>)?,
                &CheckMenuItem::with_id(app, "autostart-toggle", "Iniciar con el sistema", true, autostart_on, None::<&str>)?,
                &MenuItem::with_id(app, "check-update", "Buscar actualización…", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "version", &version, false, None::<&str>)?,
                &MenuItem::with_id(app, "quit-app", "Salir de Labstream OS", true, None::<&str>)?,
            ],
        )?)
    };
    if let Ok(menu) = build() {
        let _ = win.popup_menu(&menu);
    }
}

// Menú de CLIC DERECHO sobre una pestaña. Nativo (la barra mide 40 px: un menú HTML dentro
// del webview quedaría recortado). Recuerda en el estado sobre qué pestaña se abrió.
fn show_tab_menu<R: Runtime>(app: &AppHandle<R>, label: &str) {
    let Some(win) = main_window(app) else { return };
    let (idx, total, actual) = {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        let Some(i) = s.tabs.iter().position(|t| t.label == label) else { return };
        s.menu_tab = Some(label.to_string());
        let actual = s.tabs[i].color.clone();
        (i, s.tabs.len(), actual)
    };
    // Los ids se arman fuera del builder para poder pasarlos como &str (igual que el resto
    // del menú) en vez de mezclar String y &str en el mismo constructor.
    let ids: Vec<String> = TAB_COLORS.iter().map(|(id, _)| format!("tab-color-{id}")).collect();
    let build = || -> tauri::Result<Menu<R>> {
        // Submenú de color, con marca en el que tiene puesto ahora.
        let mut colores: Vec<CheckMenuItem<R>> = Vec::with_capacity(TAB_COLORS.len() + 1);
        colores.push(CheckMenuItem::with_id(app, "tab-color-ninguno", "Sin color", true, actual.is_none(), None::<&str>)?);
        for ((cid, nombre), id) in TAB_COLORS.iter().zip(ids.iter()) {
            let marcado = actual.as_deref() == Some(*cid);
            colores.push(CheckMenuItem::with_id(app, id.as_str(), *nombre, true, marcado, None::<&str>)?);
        }
        let refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = colores.iter().map(|c| c as &dyn tauri::menu::IsMenuItem<R>).collect();
        let submenu = Submenu::with_items(app, "Color", true, &refs)?;
        Ok(Menu::with_items(
            app,
            &[
                &MenuItem::with_id(app, "tab-duplicate", "Duplicar pestaña", true, None::<&str>)?,
                &MenuItem::with_id(app, "tab-copy-url", "Copiar enlace", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &submenu,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "tab-close", "Cerrar pestaña", true, None::<&str>)?,
                &MenuItem::with_id(app, "tab-close-others", "Cerrar las demás", total > 1, None::<&str>)?,
                &MenuItem::with_id(app, "tab-close-right", "Cerrar las de la derecha", idx + 1 < total, None::<&str>)?,
            ],
        )?)
    };
    if let Ok(menu) = build() {
        let _ = win.popup_menu(&menu);
    }
}

// Acciones del menú de pestaña. `menu_tab` guarda sobre cuál se abrió (puede no ser la activa).
fn tab_menu_action<R: Runtime>(app: &AppHandle<R>, action: &str) {
    let target = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        s.menu_tab.clone()
    };
    let Some(label) = target else { return };
    // Las que hay que cerrar se calculan ANTES de cerrar nada (los índices se mueven).
    let (url, otras, derecha) = {
        let shell = app.state::<ShellState>();
        let s = shell.lock().unwrap();
        let Some(i) = s.tabs.iter().position(|t| t.label == label) else { return };
        (
            s.tabs[i].url.clone(),
            s.tabs.iter().filter(|t| t.label != label).map(|t| t.label.clone()).collect::<Vec<_>>(),
            s.tabs.iter().skip(i + 1).map(|t| t.label.clone()).collect::<Vec<_>>(),
        )
    };
    match action {
        "tab-duplicate" => create_tab(app, Some(url), true),
        "tab-copy-url" => {
            let _ = app.clipboard().write_text(url);
        }
        "tab-close" => close_tab(app, &label),
        "tab-close-others" => {
            for l in otras {
                close_tab(app, &l);
            }
        }
        "tab-close-right" => {
            for l in derecha {
                close_tab(app, &l);
            }
        }
        // Teñir la pestaña (o quitarle el color). Se valida contra la paleta: un id que no
        // esté en TAB_COLORS deja la pestaña sin color en vez de guardar cualquier cosa.
        a if a.starts_with("tab-color-") => {
            let elegido = &a["tab-color-".len()..];
            let nuevo = TAB_COLORS
                .iter()
                .find(|(id, _)| *id == elegido)
                .map(|(id, _)| (*id).to_string());
            {
                let shell = app.state::<ShellState>();
                let mut s = shell.lock().unwrap();
                if let Some(t) = s.tabs.iter_mut().find(|t| t.label == label) {
                    t.color = nuevo;
                }
            }
            broadcast(app);
            save_persisted(app);
        }
        _ => {}
    }
}

// Reordena las pestañas al soltar una en otra posición (arrastrar en la barra). Llega el
// orden completo de etiquetas; se conserva cuál estaba activa y se guarda la sesión.
fn reorder_tabs<R: Runtime>(app: &AppHandle<R>, order: Vec<String>) {
    {
        let shell = app.state::<ShellState>();
        let mut s = shell.lock().unwrap();
        // Solo se acepta una permutación EXACTA de las pestañas actuales (nada de perder o
        // duplicar si la barra manda algo raro).
        if order.len() != s.tabs.len() || !order.iter().all(|l| s.tabs.iter().any(|t| &t.label == l)) {
            return;
        }
        let activa = s.tabs.get(s.active).map(|t| t.label.clone());
        let mut nuevas: Vec<TabRec> = Vec::with_capacity(order.len());
        for l in &order {
            if let Some(i) = s.tabs.iter().position(|t| &t.label == l) {
                nuevas.push(s.tabs.remove(i));
            }
        }
        s.tabs = nuevas;
        if let Some(a) = activa {
            s.active = s.tabs.iter().position(|t| t.label == a).unwrap_or(0);
        }
    }
    broadcast(app);
    save_persisted(app);
}

// Activa/desactiva el arranque con el sistema y RECUERDA la elección (si no, el arranque
// la re-activaba en cada apertura).
fn toggle_autostart<R: Runtime>(app: &AppHandle<R>) {
    let mgr = app.autolaunch();
    let was_on = mgr.is_enabled().unwrap_or(false);
    let _ = if was_on { mgr.disable() } else { mgr.enable() };
    {
        let shell = app.state::<ShellState>();
        shell.lock().unwrap().autostart = Some(!was_on);
    }
    save_persisted(app);
    let _ = app
        .notification()
        .builder()
        .title("Labstream OS")
        .body(if was_on {
            "Ya no se iniciará con el sistema."
        } else {
            "Se iniciará automáticamente con el sistema."
        })
        .show();
}

// "Buscar actualización…" del menú: como la silenciosa, pero SIEMPRE responde con un aviso
// (instalando / ya estás al día / sin red).
#[cfg(desktop)]
async fn check_update_manual(handle: tauri::AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    let notify = |body: String| {
        let _ = handle.notification().builder().title("Labstream OS").body(body).show();
    };
    let Ok(updater) = handle.updater() else {
        notify("El actualizador no está disponible en esta instalación.".into());
        return;
    };
    match updater.check().await {
        Ok(Some(update)) => {
            notify(format!("Instalando la versión {}…", update.version));
            if update.download_and_install(|_, _| {}, || {}).await.is_ok() {
                handle.restart();
            } else {
                notify("No se pudo instalar la actualización. Intenta de nuevo más tarde.".into());
            }
        }
        Ok(None) => notify(format!("Estás en la última versión ({}).", handle.package_info().version)),
        Err(_) => notify("No se pudo comprobar la actualización (revisa la red).".into()),
    }
}

// Muestra y enfoca la ventana principal (tray, dock, segunda instancia).
fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = main_window(app) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// Busca e instala una actualización en segundo plano. Si instala, avisa y reinicia.
#[cfg(desktop)]
async fn check_update(handle: tauri::AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    let updater = match handle.updater() {
        Ok(u) => u,
        Err(_) => return,
    };
    if let Ok(Some(update)) = updater.check().await {
        if update.download_and_install(|_, _| {}, || {}).await.is_ok() {
            let _ = handle
                .notification()
                .builder()
                .title("Labstream OS")
                .body("Se instaló una actualización. La app se reiniciará.")
                .show();
            handle.restart();
        }
    }
}

// Extrae un campo string del payload JSON de un evento.
fn field(payload: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get(key)?
        .as_str()
        .map(|s| s.to_string())
}

fn field_i64(payload: &str, key: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(payload).ok()?.get(key)?.as_i64()
}

fn field_bool(payload: &str, key: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(payload).ok()?.get(key)?.as_bool()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Una sola instancia (solo en release: en dev conviviría con la app instalada).
    #[cfg(not(debug_assertions))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }));
    }

    builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None));

    let app = builder
        .manage(ShellState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let saved = load_persisted(&handle);

            // Arranque automático al iniciar sesión: por defecto activado, pero se respeta
            // lo que el usuario elija en el menú ⋮ ("Iniciar con el sistema").
            match saved.autostart {
                Some(false) => {
                    let _ = app.autolaunch().disable();
                }
                _ => {
                    let _ = app.autolaunch().enable();
                }
            }

            // Al abrir, busca una actualización en segundo plano (no bloquea el arranque).
            #[cfg(desktop)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(check_update(handle));
            }

            {
                let shell = handle.state::<ShellState>();
                let mut s = shell.lock().unwrap();
                s.zoom_idx = saved.zoom_idx.unwrap_or(ZOOM_100).min(ZOOM_STEPS.len() - 1);
                s.win = saved.win;
                s.autostart = saved.autostart;
                s.dark = saved.dark;
            }

            // Ventana principal (contenedora). Tamaño/posición de la sesión anterior si los hay.
            // En AMBOS sistemas la barra de pestañas ocupa la fila del título, como Chrome:
            // no se desperdicia una franja entera en repetir el nombre de la app.
            // - macOS: título en OVERLAY y sin texto; el semáforo lo sigue pintando el sistema
            //   encima y la barra le deja su hueco (clase `mac` del CSS).
            // - Windows: ventana SIN decoración; los botones minimizar/maximizar/cerrar los
            //   dibuja la propia barra (clase `win`) y actúan por el evento `ls-window`.
            //   `resizable(true)` conserva WS_THICKFRAME, así que los bordes siguen
            //   redimensionando y Aero Snap (arrastrar a un borde, Win+flechas) sigue vivo.
            let mut wb = WindowBuilder::new(app, "main")
                .title("Labstream OS")
                .resizable(true)
                .min_inner_size(1024.0, 700.0);
            #[cfg(target_os = "macos")]
            {
                wb = wb.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
            }
            #[cfg(target_os = "windows")]
            {
                wb = wb.decorations(false);
            }
            match saved.win {
                Some(r) if r.w >= 600.0 && r.h >= 400.0 => {
                    wb = wb.inner_size(r.w, r.h).position(r.x, r.y);
                }
                _ => {
                    wb = wb.inner_size(1400.0, 900.0).center();
                }
            }
            let win = wb.build()?;

            // Menú nativo SOLO en macOS: da Cmd+C/V (Edición), y los atajos de pestañas/zoom
            // como aceleradores de menú. En Windows no ponemos menú (los atajos van por JS).
            #[cfg(target_os = "macos")]
            {
                let h = app.handle();
                let app_menu = Submenu::with_items(
                    h,
                    "Labstream OS",
                    true,
                    &[
                        &MenuItem::with_id(h, "check-update", "Buscar actualización…", true, None::<&str>)?,
                        &PredefinedMenuItem::separator(h)?,
                        &PredefinedMenuItem::hide(h, Some("Ocultar Labstream OS"))?,
                        &PredefinedMenuItem::hide_others(h, Some("Ocultar otros"))?,
                        &PredefinedMenuItem::show_all(h, Some("Mostrar todo"))?,
                        &PredefinedMenuItem::separator(h)?,
                        &PredefinedMenuItem::quit(h, Some("Salir de Labstream OS"))?,
                    ],
                )?;
                let archivo = Submenu::with_items(
                    h,
                    "Archivo",
                    true,
                    &[
                        &MenuItem::with_id(h, "new-tab", "Nueva pestaña", true, Some("CmdOrCtrl+T"))?,
                        &MenuItem::with_id(h, "close-tab", "Cerrar pestaña", true, Some("CmdOrCtrl+W"))?,
                        &MenuItem::with_id(h, "reopen-tab", "Reabrir pestaña cerrada", true, Some("CmdOrCtrl+Shift+T"))?,
                        &PredefinedMenuItem::separator(h)?,
                        &MenuItem::with_id(h, "open-browser", "Abrir esta página en el navegador", true, None::<&str>)?,
                    ],
                )?;
                let edicion = Submenu::with_items(
                    h,
                    "Edición",
                    true,
                    &[
                        &PredefinedMenuItem::undo(h, Some("Deshacer"))?,
                        &PredefinedMenuItem::redo(h, Some("Rehacer"))?,
                        &PredefinedMenuItem::separator(h)?,
                        &PredefinedMenuItem::cut(h, Some("Cortar"))?,
                        &PredefinedMenuItem::copy(h, Some("Copiar"))?,
                        &PredefinedMenuItem::paste(h, Some("Pegar"))?,
                        &PredefinedMenuItem::select_all(h, Some("Seleccionar todo"))?,
                    ],
                )?;
                let vista = Submenu::with_items(
                    h,
                    "Vista",
                    true,
                    &[
                        &MenuItem::with_id(h, "reload", "Recargar", true, Some("CmdOrCtrl+R"))?,
                        &PredefinedMenuItem::separator(h)?,
                        &MenuItem::with_id(h, "zoom-in", "Acercar", true, Some("CmdOrCtrl+="))?,
                        &MenuItem::with_id(h, "zoom-out", "Alejar", true, Some("CmdOrCtrl+-"))?,
                        &MenuItem::with_id(h, "zoom-0", "Tamaño real", true, Some("CmdOrCtrl+0"))?,
                        &PredefinedMenuItem::separator(h)?,
                        &PredefinedMenuItem::fullscreen(h, Some("Pantalla completa"))?,
                    ],
                )?;
                let historial = Submenu::with_items(
                    h,
                    "Historial",
                    true,
                    &[
                        &MenuItem::with_id(h, "nav-back", "Atrás", true, Some("CmdOrCtrl+["))?,
                        &MenuItem::with_id(h, "nav-forward", "Adelante", true, Some("CmdOrCtrl+]"))?,
                    ],
                )?;
                let ventana = Submenu::with_items(
                    h,
                    "Ventana",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(h, Some("Minimizar"))?,
                        &PredefinedMenuItem::maximize(h, Some("Zoom"))?,
                    ],
                )?;
                let menu = Menu::with_items(h, &[&app_menu, &archivo, &edicion, &vista, &historial, &ventana])?;
                app.set_menu(menu)?;
            }

            // Manejador de TODOS los menús (el de macOS y el popup ⋮ de ambas plataformas).
            app.on_menu_event(|app, event| match event.id().as_ref() {
                "nav-back" => nav_tab(app, -1),
                "nav-forward" => nav_tab(app, 1),
                "new-tab" => create_tab(app, None, true),
                "close-tab" => {
                    if let Some(l) = active_label(app) {
                        close_tab(app, &l);
                    }
                }
                "reopen-tab" => reopen_tab(app),
                "reload" => reload_tab(app, None),
                "zoom-in" => zoom_step(app, 1),
                "zoom-out" => zoom_step(app, -1),
                "zoom-0" => zoom_step(app, 0),
                "open-browser" => {
                    if let Some(url) = active_url(app) {
                        let _ = app.opener().open_url(url, None::<&str>);
                    }
                }
                id @ ("tab-duplicate" | "tab-copy-url" | "tab-close" | "tab-close-others" | "tab-close-right") => {
                    tab_menu_action(app, id)
                }
                id if id.starts_with("tab-color-") => tab_menu_action(app, id),
                "autostart-toggle" => toggle_autostart(app),
                "check-update" => {
                    #[cfg(desktop)]
                    tauri::async_runtime::spawn(check_update_manual(app.clone()));
                }
                "quit-app" => {
                    save_persisted(app);
                    app.exit(0);
                }
                _ => {}
            });

            // Barra de pestañas (webview local, arriba).
            let scale = win.scale_factor().unwrap_or(1.0);
            let size = win.inner_size()?.to_logical::<f64>(scale);
            win.add_child(
                WebviewBuilder::new(CHROME, WebviewUrl::App("index.html".into()))
                    // Sin esto Tauri instala su manejador de arrastre del SISTEMA en el
                    // webview y se traga los eventos HTML5 → no se podrían reordenar las
                    // pestañas arrastrando. La barra no recibe archivos, así que sobra.
                    .disable_drag_drop_handler(),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, TAB_BAR_H),
            )?;

            // Pestañas de la sesión anterior (o una nueva si no hay), cada una con su color.
            // El color se empareja con su URL ANTES de filtrar: el filtro por origen puede
            // descartar entradas y entonces los índices de las dos listas dejarían de casar.
            let restore: Vec<(String, String)> = {
                let server = Url::parse(SERVER_URL).expect("URL del servidor válida");
                saved
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, u)| Url::parse(u).map(|p| p.origin() == server.origin()).unwrap_or(false))
                    .take(12)
                    .map(|(i, u)| (u.clone(), saved.colors.get(i).cloned().unwrap_or_default()))
                    .collect()
            };
            if restore.is_empty() {
                create_tab(&handle, None, true);
            } else {
                for (url, color) in &restore {
                    create_tab(&handle, Some(url.clone()), false);
                    if !color.is_empty() {
                        // create_tab acaba de empujar su pestaña al final de la lista.
                        let shell = handle.state::<ShellState>();
                        let mut s = shell.lock().unwrap();
                        if let Some(t) = s.tabs.last_mut() {
                            if t.url == *url {
                                t.color = Some(color.clone());
                            }
                        }
                    }
                }
                let idx = saved.active.min(restore.len() - 1);
                let label = {
                    let shell = handle.state::<ShellState>();
                    let s = shell.lock().unwrap();
                    s.tabs.get(idx).map(|t| t.label.clone())
                };
                if let Some(l) = label {
                    activate_tab(&handle, &l);
                }
            }

            // ── Eventos del shell (desde la barra y desde las pestañas) ──
            let h = handle.clone();
            handle.listen_any("ls-ready", move |_| {
                layout_all(&h);
                broadcast(&h);
            });
            let h = handle.clone();
            handle.listen_any("ls-new-tab", move |e| {
                let url = field(e.payload(), "url");
                create_tab(&h, url, true);
            });
            let h = handle.clone();
            handle.listen_any("ls-activate", move |e| {
                if let Some(l) = field(e.payload(), "label") {
                    activate_tab(&h, &l);
                }
            });
            let h = handle.clone();
            handle.listen_any("ls-close-tab", move |e| {
                let label = field(e.payload(), "label").or_else(|| active_label(&h));
                if let Some(l) = label {
                    close_tab(&h, &l);
                }
            });
            let h = handle.clone();
            handle.listen_any("ls-reload", move |e| {
                reload_tab(&h, field(e.payload(), "label"));
            });
            let h = handle.clone();
            handle.listen_any("ls-zoom", move |e| {
                let dir = field_i64(e.payload(), "dir").unwrap_or(0) as i32;
                zoom_step(&h, dir);
            });
            let h = handle.clone();
            handle.listen_any("ls-cycle", move |e| {
                let dir = field_i64(e.payload(), "dir").unwrap_or(1) as i32;
                cycle_tab(&h, dir);
            });
            let h = handle.clone();
            handle.listen_any("ls-activate-n", move |e| {
                if let Some(n) = field_i64(e.payload(), "n") {
                    activate_nth(&h, n.max(1) as usize);
                }
            });
            let h = handle.clone();
            handle.listen_any("ls-nav", move |e| {
                let dir = field_i64(e.payload(), "dir").unwrap_or(-1) as i32;
                nav_tab(&h, dir);
            });
            let h = handle.clone();
            handle.listen_any("ls-reopen", move |_| {
                reopen_tab(&h);
            });
            // Globo de no-leídos: lo emite la web app cuando cambia el conteo del chat.
            let h = handle.clone();
            handle.listen_any("ls-badge", move |e| {
                let n = field_i64(e.payload(), "count").unwrap_or(0);
                set_badge(&h, n);
            });
            let h = handle.clone();
            let h_tabmenu = handle.clone();
            handle.listen_any("ls-tab-menu", move |e| {
                if let Some(label) = field(e.payload(), "label") {
                    show_tab_menu(&h_tabmenu, &label);
                }
            });
            let h_reorder = handle.clone();
            handle.listen_any("ls-reorder", move |e| {
                // `order` llega como lista de etiquetas separadas por coma (el payload de los
                // eventos del shell es plano: strings y números).
                if let Some(orden) = field(e.payload(), "order") {
                    let v: Vec<String> = orden.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    reorder_tabs(&h_reorder, v);
                }
            });
            handle.listen_any("ls-menu", move |_| {
                show_app_menu(&h);
            });
            // Botones de ventana de la barra. Solo se pintan en Windows, donde la ventana va
            // sin decoración y no hay aspa del sistema. «close» pasa por CloseRequested, así
            // que hace lo mismo de siempre: ocultar a la bandeja, no salir de la app.
            let h = handle.clone();
            handle.listen_any("ls-window", move |e| {
                let Some(win) = main_window(&h) else { return };
                match field(e.payload(), "act").as_deref() {
                    Some("min") => {
                        let _ = win.minimize();
                    }
                    Some("max") => {
                        // El repintado del icono ▢/❐ lo dispara el Resized que viene después.
                        if win.is_maximized().unwrap_or(false) {
                            let _ = win.unmaximize();
                        } else {
                            let _ = win.maximize();
                        }
                    }
                    Some("close") => {
                        let _ = win.close();
                    }
                    _ => {}
                }
            });
            // Clic en la etiqueta de versión de la barra → buscar actualización (con aviso).
            let h = handle.clone();
            handle.listen_any("ls-check-update", move |_| {
                #[cfg(desktop)]
                tauri::async_runtime::spawn(check_update_manual(h.clone()));
            });
            let h = handle.clone();
            handle.listen_any("ls-title", move |e| {
                let (label, title, url, dark) = (
                    field(e.payload(), "label"),
                    field(e.payload(), "title"),
                    field(e.payload(), "url"),
                    field_bool(e.payload(), "dark"),
                );
                if let Some(label) = label {
                    let shell = h.state::<ShellState>();
                    let mut s = shell.lock().unwrap();
                    if let Some(t) = s.tabs.iter_mut().find(|t| t.label == label) {
                        if let Some(title) = title {
                            t.title = title;
                        }
                        if let Some(url) = url {
                            t.url = url;
                        }
                    }
                    // El tema es global (todas las pestañas del origen comparten modo): el
                    // último reporte manda, venga de la pestaña que venga.
                    if let Some(dark) = dark {
                        s.dark = Some(dark);
                    }
                    drop(s);
                    broadcast(&h);
                    save_persisted(&h);
                }
            });

            // Icono en la bandeja del sistema con menú Abrir / Salir.
            let abrir = MenuItem::with_id(app, "abrir", "Abrir Labstream", true, None::<&str>)?;
            let salir = MenuItem::with_id(app, "salir", "Salir", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&abrir, &salir])?;

            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Labstream OS")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "abrir" => show_main(app),
                    "salir" => {
                        save_persisted(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Clic izquierdo en el icono → mostrar la ventana.
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // Cerrar la ventana = ocultarla a la bandeja (la app sigue notificando).
                WindowEvent::CloseRequested { api, .. } => {
                    save_persisted(window.app_handle());
                    let _ = window.hide();
                    api.prevent_close();
                }
                // La barra y las pestañas siguen el tamaño de la ventana.
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    let app = window.app_handle();
                    layout_all(app);
                    // Maximizar/restaurar llega como un Resized más: si el estado cambió, la
                    // barra tiene que repintar su icono ▢/❐. Se compara antes de emitir para
                    // no repintarla en cada píxel mientras se arrastra un borde.
                    let maxi = window.is_maximized().unwrap_or(false);
                    let cambio = {
                        let shell = app.state::<ShellState>();
                        let mut s = shell.lock().unwrap();
                        let antes = s.maximized;
                        s.maximized = maxi;
                        antes != maxi
                    };
                    if cambio {
                        broadcast(app);
                    }
                    // La geometría MAXIMIZADA no se guarda: al reabrir volvería como ventana
                    // normal del tamaño de la pantalla y, sin decoración, taparía la barra de
                    // tareas. Se conserva la última geometría restaurada.
                    if !maxi {
                        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
                            let scale = window.scale_factor().unwrap_or(1.0);
                            let p = pos.to_logical::<f64>(scale);
                            let s = size.to_logical::<f64>(scale);
                            let shell = app.state::<ShellState>();
                            shell.lock().unwrap().win =
                                Some(WinRect { x: p.x, y: p.y, w: s.width, h: s.height });
                        }
                    }
                }
                WindowEvent::Moved(_) => {
                    let app = window.app_handle();
                    if window.is_maximized().unwrap_or(false) {
                        return; // ver arriba: maximizada no se guarda
                    }
                    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
                        let scale = window.scale_factor().unwrap_or(1.0);
                        let p = pos.to_logical::<f64>(scale);
                        let s = size.to_logical::<f64>(scale);
                        let shell = app.state::<ShellState>();
                        shell.lock().unwrap().win =
                            Some(WinRect { x: p.x, y: p.y, w: s.width, h: s.height });
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error al arrancar Labstream OS");

    app.run(|app, event| {
        // macOS: clic en el icono del Dock con la ventana oculta → reabrirla.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main(app);
        }
        // Al salir del todo, guarda el estado (pestañas, zoom, ventana).
        if let tauri::RunEvent::ExitRequested { .. } = event {
            save_persisted(app);
        }
        let _ = app;
    });
}
