// Punto de entrada de la app de escritorio de Labstream OS.
// Es un "envoltorio": la ventana carga directamente el servidor (os.labstreamsas.com),
// así que aquí no hay UI propia. Sí añadimos comportamiento de cliente nativo:
//   - plugin `opener`: enlaces externos abren el navegador del sistema.
//   - plugin `notification`: la web (vía window.__TAURI__) muestra avisos nativos.
//   - plugin `autostart`: la app arranca al iniciar sesión.
//   - bandeja (tray) + "cerrar = ocultar": la app sigue corriendo y notificando
//     aunque cierres la ventana.
//   - la ventana se crea AQUÍ (no en tauri.conf.json) para poder inyectar un script
//     que manda los enlaces externos al navegador y para permitir descargas.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

const SERVER_URL: &str = "https://os.labstreamsas.com";

// Script que corre en CADA carga de página del servidor. Reenvía al NAVEGADOR del sistema los
// enlaces a OTRO origen (Drive, documentos externos, etc.): clics en `<a>` externos, `target=_blank`
// externos y `window.open(url)` externos. NO toca la navegación del propio origen, así el login por
// Authentik —que redirige a otro dominio y vuelve— sigue funcionando. Usa el comando del plugin
// opener, permitido por la capability `opener:default` para este origen remoto.
const INIT_JS: &str = r#"
(function () {
  if (window.__lsExternalLinks) return;
  window.__lsExternalLinks = true;
  var APP_ORIGIN = location.origin;
  function toAbs(href) { try { return new URL(href, location.href); } catch (e) { return null; } }
  function isExternal(u) { return !!u && (u.protocol === 'http:' || u.protocol === 'https:') && u.origin !== APP_ORIGIN; }
  function openExternal(url) {
    try { window.__TAURI__.core.invoke('plugin:opener|open_url', { url: url, with: null }); }
    catch (e) { try { window.__TAURI__.opener.openUrl(url); } catch (_) {} }
  }
  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var u = toAbs(a.getAttribute('href'));
    if (isExternal(u)) { e.preventDefault(); openExternal(u.href); }
  }, true);
  var _open = window.open;
  window.open = function (url, name, feats) {
    var u = url ? toAbs(url) : null;
    if (isExternal(u)) { openExternal(u.href); return null; }
    return _open ? _open.call(window, url, name, feats) : null;
  };
})();
"#;

// Muestra y enfoca la ventana principal (desde el tray o su menú).
fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // Arranque automático al iniciar sesión (idempotente).
            let _ = app.autolaunch().enable();

            // Ventana principal: carga el servidor del NAS. Se crea aquí (no en el config) para
            // poder inyectar el script de enlaces externos y permitir las descargas.
            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(SERVER_URL.parse().expect("URL del servidor válida")),
            )
            .title("Labstream OS")
            .inner_size(1400.0, 900.0)
            .min_inner_size(1024.0, 700.0)
            .resizable(true)
            .center()
            .initialization_script(INIT_JS)
            // Deja pasar las descargas (entregables, exportaciones) con el diálogo nativo de guardado.
            .on_download(|_webview, _event| true)
            .build()?;

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
                    "salir" => app.exit(0),
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
        // Cerrar la ventana = ocultarla a la bandeja (la app sigue notificando).
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error al arrancar Labstream OS");
}
