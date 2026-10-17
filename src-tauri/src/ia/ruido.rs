// ── Reducir ruido con IA (1.31): NAFNet sobre la foto entera, a trozos con solape ──────────────
//
// La página (lib/revelado/ruido-ia en labstream-os) manda una franja de la foto ya preparada para
// el modelo —RGB de 16 bits en sRGB, con las sombras levantadas por una curva que sabe deshacer— y
// recibe la misma franja limpia, también en 16 bits. Aquí solo se trocea: el modelo (NAFNet SIDD,
// exportación ONNX de darktable-ai) tiene la entrada fija de 768 × 768, así que la franja se corre a
// trozos de LADO que se pisan SOLAPE puntos y se funden con un peso que baja hacia los bordes (sin
// costuras). Un lado más corto que LADO se completa en espejo.

/// El lado de la entrada del modelo (fijo en su exportación).
pub const LADO: usize = 768;
/// Lo que se pisan dos trozos vecinos (lo de darktable: OVERLAP_DENOISE).
pub const SOLAPE: usize = 64;

/// Dónde empieza cada trozo a lo largo de un lado de `n` puntos (el último, pegado al final).
pub fn posiciones(n: usize, lado: usize, solape: usize) -> Vec<usize> {
    if n <= lado {
        return vec![0];
    }
    let paso = lado - solape;
    let mut p: Vec<usize> = (0..=(n - lado)).step_by(paso).collect();
    if *p.last().unwrap() != n - lado {
        p.push(n - lado);
    }
    p
}

/// El índice `i` (puede caer fuera, por un lado corto) dentro de 0..n, en espejo.
fn espejo(i: usize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let periodo = 2 * (n - 1);
    let m = i % periodo;
    if m < n {
        m
    } else {
        periodo - m
    }
}

/// El peso de cada punto de un trozo a lo largo de un lado: sube en el solape y es 1 en el centro.
/// Contra el borde de la foto no baja (ahí no hay vecino con el que fundirse).
fn rampa(lado: usize, solape: usize, inicio: bool, fin: bool) -> Vec<f32> {
    (0..lado)
        .map(|i| {
            let f = i as f32 + 0.5;
            let a = if inicio { 1.0 } else { (f / solape as f32).min(1.0) };
            let b = if fin { 1.0 } else { ((lado as f32 - f) / solape as f32).min(1.0) };
            a * b
        })
        .collect()
}

/// Limpia `rgb` (RGB de 16 bits, `ancho` × `alto`) con `modelo`, que recibe un trozo [1,3,LADO,LADO]
/// en 0..1 y devuelve lo mismo limpio. `avance` recibe la parte hecha (0..1).
pub fn limpiar(
    rgb: &[u16],
    ancho: usize,
    alto: usize,
    modelo: &mut dyn FnMut(Vec<f32>) -> Result<Vec<f32>, String>,
    avance: &mut dyn FnMut(f64),
) -> Result<Vec<u16>, String> {
    let plano = LADO * LADO;
    let xs = posiciones(ancho, LADO, SOLAPE);
    let ys = posiciones(alto, LADO, SOLAPE);
    let total = xs.len() * ys.len();
    let mut suma = vec![0f32; ancho * alto * 3];
    let mut peso = vec![0f32; ancho * alto];
    let mut hechos = 0;
    for (iy, &y0) in ys.iter().enumerate() {
        let ry = rampa(LADO, SOLAPE, iy == 0, iy + 1 == ys.len());
        for (ix, &x0) in xs.iter().enumerate() {
            let rx = rampa(LADO, SOLAPE, ix == 0, ix + 1 == xs.len());
            let mut trozo = vec![0f32; 3 * plano];
            for ty in 0..LADO {
                let y = espejo(y0 + ty, alto);
                for tx in 0..LADO {
                    let x = espejo(x0 + tx, ancho);
                    let i = (y * ancho + x) * 3;
                    for c in 0..3 {
                        trozo[c * plano + ty * LADO + tx] = rgb[i + c] as f32 / 65535.0;
                    }
                }
            }
            let limpio = modelo(trozo)?;
            if limpio.len() != 3 * plano {
                return Err(format!("El modelo contestó {} valores y se esperaban {}.", limpio.len(), 3 * plano));
            }
            // solo lo que cae dentro de la foto (lo del espejo se descarta)
            for ty in 0..LADO.min(alto - y0) {
                let y = y0 + ty;
                for tx in 0..LADO.min(ancho - x0) {
                    let x = x0 + tx;
                    let w = ry[ty] * rx[tx];
                    let j = y * ancho + x;
                    peso[j] += w;
                    for c in 0..3 {
                        suma[j * 3 + c] += limpio[c * plano + ty * LADO + tx] * w;
                    }
                }
            }
            hechos += 1;
            avance(hechos as f64 / total as f64);
        }
    }
    Ok(suma
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = peso[i / 3];
            let v = if w > 0.0 { s / w } else { 0.0 };
            (v.clamp(0.0, 1.0) * 65535.0).round() as u16
        })
        .collect())
}

/// La respuesta de `ia_ruido`: [ancho u32][alto u32][RGB de 16 bits] (little endian).
pub fn respuesta(ancho: usize, alto: usize, rgb: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + rgb.len() * 2);
    out.extend_from_slice(&(ancho as u32).to_le_bytes());
    out.extend_from_slice(&(alto as u32).to_le_bytes());
    for v in rgb {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Los bytes del cuerpo (RGB de 16 bits, little endian) a muestras.
pub fn muestras(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn los_trozos_cubren_el_lado_y_el_ultimo_va_pegado_al_final() {
        assert_eq!(posiciones(500, 768, 64), vec![0]);
        assert_eq!(posiciones(768, 768, 64), vec![0]);
        assert_eq!(posiciones(769, 768, 64), vec![0, 1]);
        let p = posiciones(4256, 768, 64);
        assert_eq!(p[0], 0);
        assert_eq!(*p.last().unwrap(), 4256 - 768);
        assert!(p.windows(2).all(|w| w[1] - w[0] <= 768 - 64));
    }

    #[test]
    fn el_espejo_no_se_sale() {
        assert_eq!(espejo(0, 5), 0);
        assert_eq!(espejo(4, 5), 4);
        assert_eq!(espejo(5, 5), 3);
        assert_eq!(espejo(8, 5), 0);
        assert_eq!(espejo(9, 5), 1);
        assert_eq!(espejo(1000, 1), 0);
        for i in 0..3000 {
            assert!(espejo(i, 7) < 7);
        }
    }

    /// Con un «modelo» que devuelve lo mismo, la foto sale igual (los pesos suman bien y nada se corre).
    #[test]
    fn con_un_modelo_que_no_cambia_nada_la_foto_sale_igual() {
        for (ancho, alto) in [(1000usize, 900usize), (300, 2000), (5, 3), (1600, 1540)] {
            let rgb: Vec<u16> = (0..ancho * alto * 3).map(|i| ((i * 7919) % 65536) as u16).collect();
            let mut llamadas = 0;
            let out = limpiar(&rgb, ancho, alto, &mut |t| {
                llamadas += 1;
                Ok(t)
            }, &mut |_| {})
            .unwrap();
            assert_eq!(llamadas, posiciones(ancho, LADO, SOLAPE).len() * posiciones(alto, LADO, SOLAPE).len());
            let peor = rgb.iter().zip(&out).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
            assert!(peor <= 1, "{ancho}×{alto}: se apartó {peor}");
        }
    }

    #[test]
    fn la_respuesta_lleva_las_medidas_y_las_muestras() {
        let r = respuesta(2, 1, &[1, 2, 3, 65535, 0, 258]);
        assert_eq!(&r[..8], &[2, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(muestras(&r[8..]), vec![1, 2, 3, 65535, 0, 258]);
    }
}
