// ── El motor: ONNX Runtime con DirectML sobre la tarjeta (Windows) ──
//
// DirectML y no CUDA: corre en cualquier tarjeta con DirectX 12 —las NVIDIA del equipo incluidas—
// sin pedir versión de controlador, y lo que hay que bajar son ~200 MB de Microsoft (NuGet) en vez
// de ~1,3 GB de bibliotecas de NVIDIA. La exportación oficial de BiRefNet lleva la convolución
// deformable deshecha en GatherND (lo que DirectML sabe hacer; DeformConv no).
//
// Las DLL no van en el instalador: las baja `ia_preparar` (piezas.rs) a la carpeta de la IA y aquí
// se cargan desde ahí. DirectML.dll se carga ANTES por su ruta: onnxruntime.dll la pide por nombre
// y, ya cargada, Windows le da esa y no la vieja de System32. ONNX Runtime se inicia UNA vez por
// proceso (`ENTORNO`); la sesión (el modelo en la tarjeta) se abre y se suelta según se use.

#[cfg(windows)]
mod imp {
    use super::super::piezas;
    use ort::{
        ep,
        environment::Environment,
        session::{builder::GraphOptimizationLevel, Session},
        value::Tensor,
    };
    use std::path::Path;
    use std::sync::OnceLock;

    static ENTORNO: OnceLock<Result<Environment, String>> = OnceLock::new();

    fn entorno(dir: &Path) -> Result<&'static Environment, String> {
        ENTORNO
            .get_or_init(|| {
                cargar_por_ruta(&dir.join(piezas::DLL_DIRECTML))?;
                let env = ort::init_from(dir.join(piezas::DLL_ORT))
                    .map_err(|e| format!("No se pudo cargar ONNX Runtime: {e}"))?
                    .with_name("labstream-ia")
                    .build()
                    .map_err(|e| format!("No se pudo iniciar ONNX Runtime: {e}"))?;
                Ok(env)
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// Carga una DLL por su ruta completa y la deja cargada para siempre.
    fn cargar_por_ruta(ruta: &Path) -> Result<(), String> {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::System::LibraryLoader::LoadLibraryW;
        let w: Vec<u16> = ruta.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        unsafe { LoadLibraryW(PCWSTR(w.as_ptr())) }.map(|_| ()).map_err(|e| format!("No se pudo cargar {}: {e}", ruta.display()))
    }

    pub struct Motor {
        sesion: Session,
        entrada: String,
        donde: &'static str,
    }

    impl Motor {
        pub fn abrir(dir: &Path) -> Result<Motor, String> {
            Self::abrir_archivo(dir, piezas::ARCHIVO_MODELO)
        }

        pub fn abrir_archivo(dir: &Path, archivo: &str) -> Result<Motor, String> {
            let env = entorno(dir)?;
            let e = |e: ort::Error<ort::session::builder::SessionBuilder>| format!("No se pudo abrir el modelo: {e}");
            let sesion = Session::builder(env)
                .map_err(|e| format!("No se pudo abrir el modelo: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(e)?
                // lo que pide DirectML: sin patrones de memoria y en serie
                .with_memory_pattern(false)
                .map_err(e)?
                .with_parallel_execution(false)
                .map_err(e)?
                // El grafo FUNDIDO de DirectML pide la memoria de toda la red de una vez: con el modelo
                // HR no cabía ni en 16 GB (E_OUTOFMEMORY en DmlFusedNode, RTX 5080, 14-oct-2026) y en
                // media precisión iba 6 veces más lento. Nodo a nodo, cada tensor se suelta cuando ya
                // no hace falta (el general: 0,3 s).
                .with_config_entry("ep.dml.disable_graph_fusion", "1")
                .map_err(e)?
                .with_execution_providers([ep::DirectML::default()
                    .with_device_filter(ep::directml::DeviceFilter::Gpu)
                    .with_performance_preference(ep::directml::PerformancePreference::HighPerformance)
                    .build()
                    // sin la tarjeta NO se sigue en la CPU: serían 30 s por foto
                    .error_on_failure()])
                .map_err(e)?
                .commit_from_file(dir.join(archivo))
                .map_err(|e| format!("No se pudo abrir el modelo: {e}"))?;
            let entrada = sesion.inputs().first().map(|i| i.name().to_string()).ok_or("El modelo no tiene entrada.")?;
            Ok(Motor { sesion, entrada, donde: "DirectML" })
        }

        /// En la CPU (Rellenar: LaMa). Las exportaciones de LaMa no corren con DirectML —su FFT, sea el
        /// operador DFT o productos de matrices, falla con 80070057 en una RTX 5080 (14-oct-2026)— y en
        /// la CPU sí: 2,7 s a 512 px y 10 s a 1024. Y no le quita memoria a la tarjeta (Resolve).
        pub fn abrir_en_cpu(dir: &Path, archivo: &str) -> Result<Motor, String> {
            let env = entorno(dir)?;
            let e = |e: ort::Error<ort::session::builder::SessionBuilder>| format!("No se pudo abrir el modelo: {e}");
            let sesion = Session::builder(env)
                .map_err(|e| format!("No se pudo abrir el modelo: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(e)?
                .with_execution_providers([ep::CPU::default().build()])
                .map_err(e)?
                .commit_from_file(dir.join(archivo))
                .map_err(|e| format!("No se pudo abrir el modelo: {e}"))?;
            let entrada = sesion.inputs().first().map(|i| i.name().to_string()).ok_or("El modelo no tiene entrada.")?;
            Ok(Motor { sesion, entrada, donde: "CPU" })
        }

        pub fn donde(&self) -> &'static str {
            self.donde
        }

        /// La entrada [1, 3, lado, lado] → la salida [1, 1, lado, lado] (la última, si hay varias).
        pub fn correr(&mut self, entrada: Vec<f32>, lado: usize) -> Result<Vec<f32>, String> {
            let (forma, datos) = self.correr_forma(&[1, 3, lado, lado], entrada)?;
            if datos.len() != lado * lado {
                return Err(format!("El modelo contestó {forma:?} y se esperaba {lado} × {lado}."));
            }
            Ok(datos)
        }

        /// Su única entrada con esta forma → la última salida (su forma y sus datos).
        pub fn correr_forma(&mut self, forma: &[usize], entrada: Vec<f32>) -> Result<(Vec<i64>, Vec<f32>), String> {
            let t = Tensor::from_array((forma.to_vec(), entrada)).map_err(|e| format!("La foto no entró al modelo: {e}"))?;
            let nombre = self.entrada.clone();
            let salidas = self.sesion.run(ort::inputs![nombre => t]).map_err(|e| format!("La tarjeta no pudo con la foto: {e}"))?;
            let ultima = salidas.len().checked_sub(1).ok_or("El modelo no contestó.")?;
            let (forma, datos) = salidas[ultima].try_extract_tensor::<f32>().map_err(|e| format!("El modelo contestó algo raro: {e}"))?;
            Ok((forma.iter().copied().collect(), datos.to_vec()))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub struct Motor;

    impl Motor {
        pub fn abrir(_dir: &Path) -> Result<Motor, String> {
            Err("La IA del equipo es para Windows con tarjeta NVIDIA.".into())
        }
        pub fn abrir_en_cpu(_dir: &Path, _archivo: &str) -> Result<Motor, String> {
            Err("La IA del equipo es para Windows.".into())
        }
        pub fn donde(&self) -> &'static str {
            "-"
        }
        pub fn correr(&mut self, _entrada: Vec<f32>, _lado: usize) -> Result<Vec<f32>, String> {
            Err("La IA del equipo es para Windows con tarjeta NVIDIA.".into())
        }
        pub fn correr_forma(&mut self, _forma: &[usize], _entrada: Vec<f32>) -> Result<(Vec<i64>, Vec<f32>), String> {
            Err("La IA del equipo es para Windows con tarjeta NVIDIA.".into())
        }
    }
}

pub use imp::Motor;
