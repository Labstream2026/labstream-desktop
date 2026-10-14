// ── La foto como la quiere el modelo, y su respuesta como la quiere la página ──
// Lo mismo que hace el navegador (lib/revelado/modelo-sujeto: entradaDelModelo), aquí en Rust:
// la foto RGBA (como se ve en el editor) a `lado` × `lado` entre sus cuatro píxeles más cercanos,
// en tres planos RGB normalizados como ImageNet. Y la salida, a un byte por punto.

const MEDIA: [f32; 3] = [0.485, 0.456, 0.406];
const DESVIACION: [f32; 3] = [0.229, 0.224, 0.225];

/// RGBA (`ancho` × `alto`) → [1, 3, lado, lado] en un vector plano.
pub fn entrada(rgba: &[u8], ancho: usize, alto: usize, lado: usize) -> Vec<f32> {
    let plano = lado * lado;
    let mut out = vec![0f32; 3 * plano];
    for y in 0..lado {
        let fy = (((y as f32 + 0.5) / lado as f32) * alto as f32 - 0.5).clamp(0.0, (alto - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(alto - 1);
        let wy = fy - y0 as f32;
        for x in 0..lado {
            let fx = (((x as f32 + 0.5) / lado as f32) * ancho as f32 - 0.5).clamp(0.0, (ancho - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(ancho - 1);
            let wx = fx - x0 as f32;
            for c in 0..3 {
                let p = |xx: usize, yy: usize| rgba[(yy * ancho + xx) * 4 + c] as f32;
                let arriba = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * wx;
                let abajo = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * wx;
                let v = arriba + (abajo - arriba) * wy;
                out[c * plano + y * lado + x] = (v / 255.0 - MEDIA[c]) / DESVIACION[c];
            }
        }
    }
    out
}

/// ¿La salida viene sin sigmoide? (BiRefNet la aplica fuera de la red; si algo cae fuera de 0..1,
/// son logits y hay que aplicarla aquí.)
pub fn son_logits(salida: &[f32]) -> bool {
    salida.iter().any(|&v| !(-0.001..=1.001).contains(&v))
}

/// La salida del modelo a un byte por punto. `logits`: la salida va sin sigmoide (BiRefNet la
/// aplica fuera de la red).
pub fn a_bytes(salida: &[f32], logits: bool) -> Vec<u8> {
    salida
        .iter()
        .map(|&v| {
            let p = if logits { 1.0 / (1.0 + (-v).exp()) } else { v };
            (p.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect()
}

/// La respuesta de `ia_sujeto` para la página: [ancho u32][alto u32][bytes] (little endian).
pub fn respuesta(ancho: u32, alto: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + bytes.len());
    out.extend_from_slice(&ancho.to_le_bytes());
    out.extend_from_slice(&alto.to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_entrada_es_la_del_navegador() {
        // 2 × 1: rojo puro y blanco; a 2 × 2 cada columna toma su píxel
        let rgba = [255, 0, 0, 255, 255, 255, 255, 255];
        let e = entrada(&rgba, 2, 1, 2);
        let norm = |v: f32, c: usize| (v / 255.0 - MEDIA[c]) / DESVIACION[c];
        assert!((e[0] - norm(255.0, 0)).abs() < 1e-5); // R de (0,0)
        assert!((e[4] - norm(0.0, 1)).abs() < 1e-5); // G de (0,0)
        assert!((e[5] - norm(255.0, 1)).abs() < 1e-5); // G de (1,0)
        assert!((e[8 + 3] - norm(255.0, 2)).abs() < 1e-5); // B de (1,1)
        assert_eq!(e.len(), 12);
    }

    #[test]
    fn la_salida_a_bytes_y_la_respuesta() {
        assert_eq!(a_bytes(&[-20.0, 0.0, 20.0], true), vec![0, 128, 255]);
        assert_eq!(a_bytes(&[0.0, 0.5, 1.2], false), vec![0, 128, 255]);
        assert!(son_logits(&[0.2, -3.0]) && !son_logits(&[0.0, 0.5, 1.0]));
        let r = respuesta(3, 2, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&r[..8], &[3, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(r.len(), 14);
    }
}
