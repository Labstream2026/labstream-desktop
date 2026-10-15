// ── RELLENAR: el hueco pintado en el editor, rellenado por LaMa (1.23) ──
//
// La página (lib/revelado/relleno y revelado-relleno de labstream-os) manda el trozo de la foto
// alrededor de lo pintado, en 8 bits, con el hueco en el alfa (255 = rellenar). Aquí se lleva al
// tamaño del modelo —el del trozo, en múltiplos de 32, con 1024 px de lado como mucho: LaMa
// generaliza bien hasta ahí y en la CPU tarda 10 s; a 1536, 75—, se rellena, se vuelve a su tamaño y
// se devuelve el trozo con el hueco relleno y lo demás tal cual estaba (byte a byte).
//
// El modelo (LaMa, Apache-2.0; la exportación ONNX de tamaño libre de g-ronimo): una entrada
// [1, 4, alto, ancho] —RGB 0..1 con el hueco a cero, y el hueco 0/1— y una salida [1, 3, alto, ancho]
// RGB 0..1, la foto entera.

/// El lado mayor al que se rellena.
pub const LADO_MAX: usize = 1024;

/// El tamaño al que se rellena un trozo de `ancho` × `alto`: el suyo (o reducido a LADO_MAX de lado
/// mayor), en múltiplos de 32 y con 64 como mínimo.
pub fn tamano_modelo(ancho: usize, alto: usize) -> (usize, usize) {
    let mayor = ancho.max(alto) as f64;
    let k = if mayor > LADO_MAX as f64 { LADO_MAX as f64 / mayor } else { 1.0 };
    let a32 = |v: f64| ((((v * k) / 32.0).round() as usize).max(2)) * 32;
    (a32(ancho as f64), a32(alto as f64))
}

/// Remuestrea `de` (`canales` por punto, `aw` × `ah`) a `bw` × `bh` entre sus cuatro vecinos.
pub fn remuestrear(de: &[f32], aw: usize, ah: usize, canales: usize, bw: usize, bh: usize) -> Vec<f32> {
    let mut out = vec![0f32; bw * bh * canales];
    for y in 0..bh {
        let fy = (((y as f32 + 0.5) / bh as f32) * ah as f32 - 0.5).clamp(0.0, (ah - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(ah - 1);
        let wy = fy - y0 as f32;
        for x in 0..bw {
            let fx = (((x as f32 + 0.5) / bw as f32) * aw as f32 - 0.5).clamp(0.0, (aw - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(aw - 1);
            let wx = fx - x0 as f32;
            for c in 0..canales {
                let p = |xx: usize, yy: usize| de[(yy * aw + xx) * canales + c];
                let arriba = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * wx;
                let abajo = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * wx;
                out[(y * bw + x) * canales + c] = arriba + (abajo - arriba) * wy;
            }
        }
    }
    out
}

/// La entrada del modelo (planos [4, mh, mw]) a partir del RGBA de la página, y el hueco a su tamaño.
pub fn entrada(rgba: &[u8], ancho: usize, alto: usize, mw: usize, mh: usize) -> Vec<f32> {
    let n = ancho * alto;
    let mut rgb = Vec::with_capacity(n * 3);
    let mut hueco = Vec::with_capacity(n);
    for i in 0..n {
        rgb.extend_from_slice(&[rgba[i * 4] as f32 / 255.0, rgba[i * 4 + 1] as f32 / 255.0, rgba[i * 4 + 2] as f32 / 255.0]);
        hueco.push(if rgba[i * 4 + 3] >= 128 { 1.0 } else { 0.0 });
    }
    let rgb = remuestrear(&rgb, ancho, alto, 3, mw, mh);
    let hueco = remuestrear(&hueco, ancho, alto, 1, mw, mh);
    let m = mw * mh;
    let mut out = vec![0f32; 4 * m];
    for i in 0..m {
        // lo que toca el hueco, aunque sea un poco al reducir, es hueco
        let h = if hueco[i] > 0.02 { 1.0 } else { 0.0 };
        for c in 0..3 {
            out[c * m + i] = if h > 0.0 { 0.0 } else { rgb[i * 3 + c] };
        }
        out[3 * m + i] = h;
    }
    out
}

/// La salida del modelo (planos [3, mh, mw], 0..1) de vuelta a `ancho` × `alto`: dentro del hueco lo
/// rellenado; fuera, el RGB que llegó, tal cual. RGB 8 bits.
pub fn salida(planos: &[f32], mw: usize, mh: usize, rgba: &[u8], ancho: usize, alto: usize) -> Vec<u8> {
    let m = mw * mh;
    let mut intercalado = vec![0f32; m * 3];
    for i in 0..m {
        for c in 0..3 {
            intercalado[i * 3 + c] = planos[c * m + i];
        }
    }
    let grande = remuestrear(&intercalado, mw, mh, 3, ancho, alto);
    let n = ancho * alto;
    let mut out = vec![0u8; n * 3];
    for i in 0..n {
        let dentro = rgba[i * 4 + 3] >= 128;
        for c in 0..3 {
            out[i * 3 + c] = if dentro { (grande[i * 3 + c].clamp(0.0, 1.0) * 255.0).round() as u8 } else { rgba[i * 4 + c] };
        }
    }
    out
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn el_tamano_del_modelo() {
        assert_eq!(tamano_modelo(500, 300), (512, 288));
        assert_eq!(tamano_modelo(2048, 1024), (1024, 512));
        assert_eq!(tamano_modelo(20, 10), (64, 64));
        let (w, h) = tamano_modelo(1600, 1067);
        assert!(w <= 1024 && w % 32 == 0 && h % 32 == 0);
    }

    #[test]
    fn fuera_del_hueco_queda_lo_que_habia() {
        // 2 × 1: el primero es hueco
        let rgba = [10, 20, 30, 255, 40, 50, 60, 0];
        let e = entrada(&rgba, 2, 1, 64, 64);
        assert_eq!(e.len(), 4 * 64 * 64);
        // a la izquierda (el hueco) el RGB va a cero y el canal del hueco a 1
        assert_eq!(e[0], 0.0);
        assert_eq!(e[3 * 64 * 64], 1.0);
        // lo de la red: todo gris
        let planos = vec![0.5f32; 3 * 64 * 64];
        let s = salida(&planos, 64, 64, &rgba, 2, 1);
        assert_eq!(&s[..3], &[128, 128, 128]);
        assert_eq!(&s[3..], &[40, 50, 60]);
    }
}
