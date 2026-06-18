// Punto de entrada de la app de escritorio de Labstream OS.
// Es un "envoltorio": la ventana carga directamente el servidor del NAS
// (ver `app.windows[].url` en tauri.conf.json), así que aquí no hay lógica de UI.
// El plugin `opener` permite que los enlaces externos abran el navegador del sistema.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error al arrancar Labstream OS");
}
