// ── RELLENAR: el hueco pintado en el editor, rellenado por LaMa (1.23) ──
//
// La página (lib/revelado/relleno y revelado-relleno de labstream-os) manda el trozo de la foto
// alrededor de lo pintado, en 8 bits, con el hueco en el alfa (255 = rellenar). Aquí se lleva al
// tamaño del modelo, se rellena, se vuelve a su tamaño y se devuelve el trozo con el hueco relleno y
// lo demás tal cual estaba (byte a byte).
//
// El tamaño (`plan`): LaMa hace FFT de lo que ve, y la de ONNX Runtime en la CPU solo es rápida con
// lados que son potencias de 2: 512 × 512 son 2,7 s y 704 × 448 —casi los mismos píxeles— 10 s
// (14-oct-2026). Así que el trozo se reduce lo justo para caber en 512 (o en 1024 si pasa de 768 px:
// LaMa generaliza bien hasta ahí, 10 s) y se completa hasta la potencia de 2 con su propio borde en
// espejo (sin hueco: es contexto). Lo de fuera de la foto reducida se descarta al volver.
//
// El modelo (LaMa, Apache-2.0; la exportación ONNX de tamaño libre de g-ronimo): una entrada
// [1, 4, alto, ancho] —RGB 0..1 con el hueco a cero, y el hueco 0/1— y una salida [1, 3, alto, ancho]
// RGB 0..1, la foto entera.

/// El lado mayor al que se rellena.
pub const LADO_MAX: usize = 1024;

/// Cómo entra un trozo de `ancho` × `alto` al modelo: reducido a `sw` × `sh` y completado en espejo
/// hasta `pw` × `ph` (potencias de 2).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Plan {
    pub sw: usize,
    pub sh: usize,
    pub pw: usize,
    pub ph: usize,
}

pub fn plan(ancho: usize, alto: usize) -> Plan {
    let mayor = ancho.max(alto);
    let lado = if mayor <= 768 { 512 } else { LADO_MAX };
    let k = (lado as f64 / mayor as f64).min(1.0);
    let sw = ((ancho as f64 * k).round() as usize).clamp(8, lado);
    let sh = ((alto as f64 * k).round() as usize).clamp(8, lado);
    Plan { sw, sh, pw: sw.next_power_of_two().max(64), ph: sh.next_power_of_two().max(64) }
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

/// El índice en espejo de `i` dentro de 0..n (…2 1 0 1 2…n-2 n-1 n-2…).
fn espejo(i: usize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let periodo = 2 * (n - 1);
    let r = i % periodo;
    if r < n { r } else { periodo - r }
}

/// La entrada del modelo (planos [4, ph, pw]) a partir del RGBA de la página.
pub fn entrada(rgba: &[u8], ancho: usize, alto: usize, p: Plan) -> Vec<f32> {
    let n = ancho * alto;
    let mut rgb = Vec::with_capacity(n * 3);
    let mut hueco = Vec::with_capacity(n);
    for i in 0..n {
        rgb.extend_from_slice(&[rgba[i * 4] as f32 / 255.0, rgba[i * 4 + 1] as f32 / 255.0, rgba[i * 4 + 2] as f32 / 255.0]);
        hueco.push(if rgba[i * 4 + 3] >= 128 { 1.0 } else { 0.0 });
    }
    let rgb = remuestrear(&rgb, ancho, alto, 3, p.sw, p.sh);
    let hueco = remuestrear(&hueco, ancho, alto, 1, p.sw, p.sh);
    let m = p.pw * p.ph;
    let mut out = vec![0f32; 4 * m];
    for y in 0..p.ph {
        let dentro_y = y < p.sh;
        let sy = espejo(y, p.sh);
        for x in 0..p.pw {
            let dentro = dentro_y && x < p.sw;
            let sx = espejo(x, p.sw);
            let s = sy * p.sw + sx;
            // lo que toca el hueco, aunque sea un poco al reducir, es hueco; el espejo, nunca
            let h = if dentro && hueco[s] > 0.02 { 1.0 } else { 0.0 };
            let i = y * p.pw + x;
            for c in 0..3 {
                out[c * m + i] = if h > 0.0 { 0.0 } else { rgb[s * 3 + c] };
            }
            out[3 * m + i] = h;
        }
    }
    out
}

/// La salida del modelo (planos [3, ph, pw], 0..1) de vuelta a `ancho` × `alto`: dentro del hueco lo
/// rellenado; fuera, el RGB que llegó, tal cual. RGB 8 bits.
pub fn salida(planos: &[f32], p: Plan, rgba: &[u8], ancho: usize, alto: usize) -> Vec<u8> {
    let m = p.pw * p.ph;
    let mut intercalado = vec![0f32; p.sw * p.sh * 3];
    for y in 0..p.sh {
        for x in 0..p.sw {
            for c in 0..3 {
                intercalado[(y * p.sw + x) * 3 + c] = planos[c * m + y * p.pw + x];
            }
        }
    }
    let grande = remuestrear(&intercalado, p.sw, p.sh, 3, ancho, alto);
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
    fn el_plan_reduce_lo_justo_y_completa_hasta_potencias_de_2() {
        assert_eq!(plan(700, 460), Plan { sw: 512, sh: 336, pw: 512, ph: 512 });
        assert_eq!(plan(500, 300), Plan { sw: 500, sh: 300, pw: 512, ph: 512 });
        assert_eq!(plan(1400, 933), Plan { sw: 1024, sh: 682, pw: 1024, ph: 1024 });
        assert_eq!(plan(200, 100), Plan { sw: 200, sh: 100, pw: 256, ph: 128 });
        assert_eq!(plan(20, 10), Plan { sw: 20, sh: 10, pw: 64, ph: 64 });
        assert_eq!(espejo(5, 4), 1);
        assert_eq!(espejo(3, 4), 3);
    }

    #[test]
    fn fuera_del_hueco_queda_lo_que_habia() {
        // 2 × 1: el primero es hueco
        let rgba = [10, 20, 30, 255, 40, 50, 60, 0];
        let p = plan(2, 1);
        let e = entrada(&rgba, 2, 1, p);
        let m = p.pw * p.ph;
        assert_eq!(e.len(), 4 * m);
        // el hueco (0,0): RGB a cero y el canal del hueco a 1; el espejo (fuera de 2 × 1) no es hueco
        assert_eq!(e[0], 0.0);
        assert_eq!(e[3 * m], 1.0);
        assert_eq!(e[3 * m + 10], 0.0);
        // lo de la red: todo gris
        let planos = vec![0.5f32; 3 * m];
        let s = salida(&planos, p, &rgba, 2, 1);
        assert_eq!(&s[..3], &[128, 128, 128]);
        assert_eq!(&s[3..], &[40, 50, 60]);
    }
}
