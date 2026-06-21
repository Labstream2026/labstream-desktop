// Punto de entrada de la app de escritorio de Labstream OS.
// Es un "envoltorio": la ventana carga directamente el servidor (os.labstreamsas.com),
// así que aquí no hay UI propia. Sí añadimos comportamiento de cliente nativo:
//   - plugin `opener`: enlaces externos abren el navegador del sistema.
//   - plugin `notification`: la web (vía window.__TAURI__) muestra avisos nativos.
//   - plugin `autostart`: la app arranca al iniciar sesión.
//   - bandeja (tray) + "cerrar = ocultar": la app sigue corriendo y notificando
//     aunque cierres la ventana.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

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
