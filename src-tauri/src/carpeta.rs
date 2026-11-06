// «Abrir carpeta» de un disco de trabajo (v1.35.0, 6-nov-2026).
//
// Jonathan: «algo importante es poder abrir el disco desde la app del OS o el plugin del proyecto,
// por si quiero hacer algún cambio». El plugin de Resolve ya lo hace; aquí lo hace la app.
//
// La web no manda una ruta de este equipo (no la sabe, y una página no debe poder abrir cualquier
// cosa): manda lo que el OS sabe del disco —los servidores posibles (la IP de la oficina y la de
// ZeroTier), el nombre del recurso compartido y la carpeta dentro de él— por el evento
// `ls-abrir-carpeta`. Aquí se arma la ruta y se abre en el Finder o el Explorador:
//   · Windows: `\\<servidor>\<recurso>\<carpeta>`, con el primer servidor que contesta por SMB.
//   · macOS:   `/Volumes/<recurso>/<carpeta>` si el disco ya está montado; si no, se abre
//              `smb://<servidor>/<recurso>` para que el Finder lo monte (y se dice).
// Solo servidores de red privada, recursos y carpetas sin «..» ni caracteres raros: nada de lo que
// llega puede salirse del disco de trabajo.
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, PartialEq)]
pub struct Pedido {
    pub servidores: Vec<Ipv4Addr>,
    pub recurso: String,
    pub tramos: Vec<String>,
}

fn tramo_valido(t: &str) -> bool {
    !t.is_empty()
        && t != "."
        && t != ".."
        && t.chars().count() <= 180
        && !t.chars().any(|c| c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

/// Lo que manda la web, saneado. `None` = no se abre nada.
pub fn leer(payload: &str) -> Option<Pedido> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let servidores: Vec<Ipv4Addr> = v
        .get("servidores")?
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str()?.trim().parse::<Ipv4Addr>().ok())
        .filter(|ip| ip.is_private())
        .take(4)
        .collect();
    let recurso = v.get("recurso")?.as_str()?.trim().to_string();
    if servidores.is_empty() || !tramo_valido(&recurso) || recurso.chars().count() > 80 {
        return None;
    }
    let carpeta = v.get("carpeta").and_then(|c| c.as_str()).unwrap_or("");
    let tramos: Vec<String> = carpeta.split(['/', '\\']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    if tramos.len() > 12 || !tramos.iter().all(|t| tramo_valido(t)) {
        return None;
    }
    Some(Pedido { servidores, recurso, tramos })
}

/// `\\192.168.0.22\LABSTREAM\Cliente\2026\Proyecto`
pub fn ruta_unc(servidor: &Ipv4Addr, p: &Pedido) -> String {
    let mut r = format!("\\\\{}\\{}", servidor, p.recurso);
    for t in &p.tramos {
        r.push('\\');
        r.push_str(t);
    }
    r
}

/// El volumen montado de ese recurso entre los nombres de `/Volumes` (sin mirar mayúsculas; macOS
/// añade «-1» si ya había otro con ese nombre).
pub fn volumen_de<'a>(recurso: &str, montados: &'a [String]) -> Option<&'a String> {
    let r = recurso.to_lowercase();
    montados.iter().find(|m| m.to_lowercase() == r).or_else(|| montados.iter().find(|m| m.to_lowercase() == format!("{r}-1")))
}

fn contesta_smb(ip: &Ipv4Addr) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(*ip), 445), Duration::from_millis(1500)).is_ok()
}

pub enum Resultado {
    /// Se abre esta ruta del sistema.
    Ruta(String),
    /// macOS sin el disco montado: se abre esta dirección para que el Finder lo monte.
    Montar(String),
    Fallo(String),
}

/// Qué hay que abrir en ESTE equipo. Toca la red (sondea SMB) y el disco (mira /Volumes).
pub fn resolver(p: &Pedido) -> Resultado {
    if cfg!(target_os = "macos") {
        let montados: Vec<String> = std::fs::read_dir("/Volumes")
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        if let Some(v) = volumen_de(&p.recurso, &montados) {
            let mut ruta = PathBuf::from("/Volumes").join(v);
            for t in &p.tramos {
                ruta = ruta.join(t);
            }
            // La carpeta puede no existir todavía (el OS la crea en su pasada): se abre la más honda que haya.
            while !ruta.exists() && ruta != Path::new("/Volumes").join(v) {
                ruta.pop();
            }
            return Resultado::Ruta(ruta.to_string_lossy().to_string());
        }
        return match p.servidores.iter().find(|ip| contesta_smb(ip)) {
            Some(ip) => Resultado::Montar(format!("smb://{}/{}", ip, p.recurso.replace(' ', "%20"))),
            None => Resultado::Fallo("El servidor del disco no contesta desde este equipo (¿fuera de la oficina, sin ZeroTier?).".into()),
        };
    }
    if cfg!(target_os = "windows") {
        return match p.servidores.iter().find(|ip| contesta_smb(ip)) {
            Some(ip) => {
                let mut ruta = PathBuf::from(ruta_unc(ip, p));
                let raiz = PathBuf::from(format!("\\\\{}\\{}", ip, p.recurso));
                while !ruta.exists() && ruta != raiz {
                    ruta.pop();
                }
                Resultado::Ruta(ruta.to_string_lossy().to_string())
            }
            None => Resultado::Fallo("El servidor del disco no contesta desde este equipo (¿fuera de la oficina, sin ZeroTier?).".into()),
        };
    }
    Resultado::Fallo("Este sistema no sabe abrir discos de red desde la app.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_y_sanea_el_pedido() {
        let p = leer(r#"{"servidores":["192.168.0.22","10.61.5.96","8.8.8.8","labtem"],"recurso":"LABSTREAM","carpeta":"Academia/2026/ Curso de video /Tráiler"}"#).unwrap();
        assert_eq!(p.servidores, vec![Ipv4Addr::new(192, 168, 0, 22), Ipv4Addr::new(10, 61, 5, 96)]);
        assert_eq!(p.recurso, "LABSTREAM");
        assert_eq!(p.tramos, vec!["Academia", "2026", "Curso de video", "Tráiler"]);
        assert_eq!(ruta_unc(&p.servidores[0], &p), r"\\192.168.0.22\LABSTREAM\Academia\2026\Curso de video\Tráiler");
        // Sin carpeta: la raíz del recurso.
        assert_eq!(leer(r#"{"servidores":["192.168.0.22"],"recurso":"Reels"}"#).unwrap().tramos.len(), 0);
    }

    #[test]
    fn nada_que_se_salga_del_disco() {
        for malo in [
            r#"{"servidores":["8.8.8.8"],"recurso":"X","carpeta":"a"}"#,
            r#"{"servidores":["192.168.0.22"],"recurso":"..","carpeta":"a"}"#,
            r#"{"servidores":["192.168.0.22"],"recurso":"X","carpeta":"a/../../etc"}"#,
            r#"{"servidores":["192.168.0.22"],"recurso":"X","carpeta":"a/b:c"}"#,
            r#"{"servidores":["192.168.0.22"],"recurso":"X\\Y","carpeta":"a"}"#,
            r#"{"servidores":[],"recurso":"X","carpeta":"a"}"#,
            r#"{"recurso":"X"}"#,
            "no es json",
        ] {
            assert_eq!(leer(malo), None, "{malo}");
        }
    }

    #[test]
    fn el_volumen_montado_sin_mirar_mayusculas() {
        let montados = vec!["Macintosh HD".to_string(), "labstream".to_string(), "Reels-1".to_string()];
        assert_eq!(volumen_de("LABSTREAM", &montados), Some(&"labstream".to_string()));
        assert_eq!(volumen_de("Reels", &montados), Some(&"Reels-1".to_string()));
        assert_eq!(volumen_de("Danney", &montados), None);
    }
}
