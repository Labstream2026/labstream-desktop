// ── Lo que la IA baja la primera vez, de dónde, y con qué huella ──
// El motor (≈215 MB) es de las dos IA; cada modelo se baja solo cuando se usa la suya: el del sujeto
// (≈490 MB) o el de rellenar (≈210 MB).
//
// Fijado por versión y huella SHA-256 (comprobadas al bajarlo el 14-oct-2026): si algún día cambia
// lo que hay en esa dirección, no se usa.
//
// - El modelo: BiRefNet general (MIT, ZhengPeng7; el mismo del navegador) en media precisión, del
//   servidor de la app (api/modelos/sujeto-equipo, con la sesión de la pestaña). Sale de la
//   exportación ONNX oficial del autor (release v1 de su GitHub, float32) pasada a float16 con la
//   entrada y la salida en float32 (scripts/modelo-sujeto-equipo.py de labstream-os). Entrada
//   `input_image` [1, 3, 1024, 1024], salida `output_image` [1, 1, 1024, 1024] (logits). En una RTX
//   5080: 0,3 s por foto y el mismo borde que el original en la CPU.
//   El HR (2048 px, matting) se probó y NO va: su convolución deformable, deshecha en GatherND en la
//   exportación oficial, ocupa 14–15 GB con DirectML y tarda de 6 a 45 s (14-oct-2026).
// - Rellenar (1.27): el MOTOR GENERATIVO (Python 3.12 embebido + PyTorch CUDA 12.8 + diffusers,
//   3 GB; generativo.rs) y el modelo SDXL Inpainting 0.1 de diffusers en fp16 (6,9 GB; licencia
//   CreativeML OpenRAIL++), los dos del servidor de la app (api/modelos/equipo/<archivo>), zips
//   enteros a su carpeta. Antes (1.23–1.26) era LaMa en la CPU: se borra del equipo (generativo.rs).
// - ONNX Runtime 1.24.4 con DirectML (NuGet de Microsoft): onnxruntime.dll para x64.
// - DirectML 1.15.4 (NuGet de Microsoft): DirectML.dll para Windows x64 (el paquete trae también
//   las de Xbox y las de depurar: se saca por su ruta exacta).

use super::bajar::{Forma, Pieza};

pub const NOMBRE_MODELO: &str = "BiRefNet";
pub const NOMBRE_RELLENO: &str = "SDXL Inpainting";
/// El lado de la entrada y de la salida del modelo.
pub const LADO: usize = 1024;

pub const ARCHIVO_MODELO: &str = "birefnet-general-epoch_244-fp16.onnx";
pub const DLL_ORT: &str = "onnxruntime.dll";
pub const DLL_DIRECTML: &str = "DirectML.dll";

/// De qué IA se habla: la máscara de sujeto o el generativo (Rellenar con IA).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Que {
    Sujeto,
    Generativo,
}

impl Que {
    /// `que` de la página: "sujeto" (o nada), o "generativo". ("relleno", el LaMa de la 1.23–1.26, ya no existe.)
    pub fn de(v: Option<&str>) -> Option<Que> {
        match v {
            None | Some("sujeto") => Some(Que::Sujeto),
            Some("generativo") => Some(Que::Generativo),
            _ => None,
        }
    }
    pub fn nombre(self) -> &'static str {
        match self {
            Que::Sujeto => "sujeto",
            Que::Generativo => "generativo",
        }
    }
}

static GENERATIVO: [Pieza; 2] = [
    Pieza {
        nombre: "motor-generativo-1",
        url: "https://os.labstreamsas.com/api/modelos/equipo/motor-generativo-1-win-x64.zip",
        sha256: "4ab6136ef6289c81f55ec41a82eafe196d486aa2a1a65feea31b36a05a36faea",
        bytes: 2_957_377_300,
        forma: Forma::Carpeta(super::generativo::CARPETA_MOTOR),
        con_sesion: true,
    },
    Pieza {
        nombre: "sdxl-inpainting-0.1-fp16",
        url: "https://os.labstreamsas.com/api/modelos/equipo/sdxl-inpainting-0.1-fp16.zip",
        sha256: "6d1c1aa80a17009404bf8bfdcddd340dfa17297419aabec3f031666657dc6dc7",
        bytes: 6_941_221_477,
        forma: Forma::Carpeta(super::generativo::CARPETA_MODELO),
        con_sesion: true,
    },
];

static PIEZAS: [Pieza; 3] = [
    Pieza {
        nombre: "onnxruntime-directml-1.24.4",
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
        sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
        bytes: 12_458_649,
        forma: Forma::Zip(&["runtimes/win-x64/native/onnxruntime.dll", "runtimes/win-x64/native/onnxruntime_providers_shared.dll"]),
        con_sesion: false,
    },
    Pieza {
        nombre: "directml-1.15.4",
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
        sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9",
        bytes: 202_292_617,
        forma: Forma::Zip(&["bin/x64-win/DirectML.dll"]),
        con_sesion: false,
    },
    Pieza {
        nombre: "birefnet-general-epoch_244-fp16",
        url: "https://os.labstreamsas.com/api/modelos/sujeto-equipo",
        sha256: "323232ec73a04ac4d0ef8c325a75aa8d69ed7062235a7cf9941769fae4c9709f",
        bytes: 489_666_838,
        forma: Forma::Archivo(ARCHIVO_MODELO),
        con_sesion: true,
    },
];

/// Lo que necesita cada IA: el sujeto, ONNX Runtime + DirectML + su modelo; el generativo, su motor y su modelo.
pub fn de(que: Que) -> Vec<&'static Pieza> {
    match que {
        Que::Sujeto => PIEZAS.iter().collect(),
        Que::Generativo => GENERATIVO.iter().collect(),
    }
}
