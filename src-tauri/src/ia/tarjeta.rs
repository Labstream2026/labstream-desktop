// ── ¿Hay tarjeta NVIDIA en este equipo? ── (DXGI: lo que Windows sabe de sus adaptadores, sin
// cargar nada de la IA). Es lo que decide si la máscara de sujeto la busca la app o el navegador.

/// Una tarjeta NVIDIA del equipo: su nombre y su memoria propia (bytes).
pub struct Tarjeta {
    pub nombre: String,
    pub memoria: u64,
}

const NVIDIA: u32 = 0x10DE;

#[cfg(windows)]
pub fn nvidia() -> Option<Tarjeta> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let mut mejor: Option<Tarjeta> = None;
    for i in 0.. {
        let Ok(a) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(d) = (unsafe { a.GetDesc1() }) else { continue };
        if d.VendorId != NVIDIA || (d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
            continue;
        }
        let largo = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
        let t = Tarjeta { nombre: String::from_utf16_lossy(&d.Description[..largo]).trim().to_string(), memoria: d.DedicatedVideoMemory as u64 };
        // con dos, la de más memoria
        if mejor.as_ref().map(|m| t.memoria > m.memoria).unwrap_or(true) {
            mejor = Some(t);
        }
    }
    mejor
}

#[cfg(not(windows))]
pub fn nvidia() -> Option<Tarjeta> {
    None
}
