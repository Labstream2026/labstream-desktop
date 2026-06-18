// Evita que se abra una consola de Windows detrás de la ventana en la build de release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    labstream_desktop_lib::run()
}
