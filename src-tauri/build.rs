fn main() {
    // Los comandos de la app (la IA del equipo, src/ia; la lista de Resolve, src/resolve_lista.rs): con su manifiesto, cada uno necesita su
    // permiso en capabilities/default.json, y es lo que deja que la página remota los llame.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["ia_estado", "ia_preparar", "ia_sujeto", "ia_generar", "ia_mantener", "ia_ruido", "resolve_lista", "resolve_lista_agregar"])),
    )
    .expect("tauri-build");
}
