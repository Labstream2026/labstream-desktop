# ── El MOTOR GENERATIVO de Labstream OS (15-oct-2026) ──
# Un servidor HTTP en 127.0.0.1 que la app de escritorio arranca en cada PC con tarjeta NVIDIA
# (src/ia/generativo.rs; este archivo va dentro de la app y se copia a la carpeta del motor al
# arrancar, así cambia con la app sin volver a bajar los 4,7 GB del motor). Rellenar con IA le manda
# el trozo de la foto con lo pintado y una frase, y SDXL Inpainting (fp16, en la tarjeta) devuelve
# propuestas. Solo escucha en el equipo; nada sale de él. Si la app que lo arrancó se cierra, se
# cierra solo.
#
# El modelo y la memoria (v4, 1.29): el modelo vive en la RAM y sube a la tarjeta POR PARTES solo
# mientras genera (los codificadores de texto, la red y el decodificador, uno a la vez; el
# decodificador, por tiras): a 1 MP el pico es de unos 6 GB y, quieto, no ocupa nada de la tarjeta.
# Medido el 15-oct-2026 en una RTX 5080 de 16 GB con navegadores y video abiertos: todo en la
# tarjeta (como la v3) la llenaba y Windows la desbordaba a la RAM, y una propuesta tardaba
# minutos; por partes, 9 s. Los editores trabajan con Resolve abierto: la VRAM es de todos.
# Se abre al abrir Rellenar en la página (`/abrir`, la precarga: leer 7 GB del disco es lo que
# tarda) o con el primer relleno; mientras el panel está abierto la página lo mantiene
# (`/mantener`, cada minuto); a los SOLTAR_TRAS segundos sin uso se suelta también de la RAM.
#
#   GET  /salud            → {ok, version, modelo: "abierto"|"cerrado", vram, capacidades}
#   POST /abrir            → abre el modelo (en la RAM, listo para la tarjeta): {ok, segundos}
#   POST /mantener         → «sigue haciendo falta»: no lo suelta todavía
#   POST /rellenar (JSON)  → {imagenes: [rgb base64 crudo…], ancho, alto, segundos, semilla}
#        {rgba: base64 crudo (ancho × alto × 4; el alfa 255 = rellenar), ancho, alto,
#         frase, negativa?, n?: 1..4, pasos?: 10..40, semilla?, fuerza?: 0..1, guia?: 1..15,
#         color?: false (sin corregir el color, para comparar),
#         prellenar?: true (para quitar: el hueco se tapa con los colores de su borde antes de generar)}
#        Las propuestas salen de una en una (cabe con Resolve abierto) y con el color corregido.
#   POST /cerrar           → suelta el modelo
#   POST /salir            → termina el proceso
#
# Se arranca con: python\python.exe -I servidor.py <puerto (0 = el que haya libre)> <carpeta del
# modelo> [pid de la app]; contesta en la primera línea «puerto <n>».

import base64, io, json, os, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "4"
# Lo que sabe hacer esta versión (la página lo lee por la app, ia_estado → capacidades).
CAPACIDADES = ["abrir", "mantener", "por-partes", "una-a-una", "color", "lado-1536", "prellenar"]
SOLTAR_TRAS = int(os.environ.get("LS_SOLTAR_TRAS", str(10 * 60)))
# Lo más que se le pide al modelo: el lado mayor y, sobre todo, el área (SDXL está hecho para ~1 MP).
LADO_MAX = 1536
AREA_MAX = int(1024 * 1024 * 1.15)
# El anillo alrededor del hueco donde se mide cuánto corrió el color el modelo (px), y el de donde
# se toman los colores para tapar el hueco al quitar (más ancho: que una mota no tiña el tapado).
ANILLO = 8
ANILLO_QUITAR = 12
# El anillo de donde se mide el grano de la foto para el tapado (px).
ANILLO_GRANO = 32
PUERTO = int(sys.argv[1]) if len(sys.argv) > 1 else 0
MODELO = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "modelo")
PADRE = int(sys.argv[3]) if len(sys.argv) > 3 else 0

estado = {"pipe": None, "usado": 0.0, "ocupado": threading.Lock()}


class SinMemoria(Exception):
    pass


# Lo que se escribe va a la app por una tubería, en UTF-8 (sin esto, en Windows salía en cp1252 y
# una «·» rompía al lector); y si la tubería se cierra, el motor sigue: un registro no tumba un relleno.
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass


def log(msg):
    try:
        print(f"{time.strftime('%H:%M:%S')} {msg}", flush=True)
    except Exception:
        pass


def vram():
    try:
        import torch
        if not torch.cuda.is_available():
            return None
        libre, total = torch.cuda.mem_get_info()
        return {"libre": libre, "total": total}
    except Exception:
        return None


def abrir():
    """El modelo abierto: en la RAM, y a la tarjeta por partes solo mientras genera."""
    if estado["pipe"] is None:
        import torch
        from diffusers import StableDiffusionXLInpaintPipeline
        t = time.time()
        pipe = StableDiffusionXLInpaintPipeline.from_pretrained(MODELO, torch_dtype=torch.float16, variant="fp16", use_safetensors=True, local_files_only=True)
        pipe.set_progress_bar_config(disable=True)
        # el decodificador por tiras (a 1 MP era el pico) y cada parte a la tarjeta solo cuando trabaja
        pipe.vae.enable_tiling()
        pipe.enable_model_cpu_offload()
        estado["pipe"] = pipe
        log(f"modelo abierto en {time.time() - t:.1f} s")
    estado["usado"] = time.time()
    return estado["pipe"]


def cerrar():
    if estado["pipe"] is not None:
        import gc
        import torch
        estado["pipe"] = None
        gc.collect()
        torch.cuda.empty_cache()
        log("modelo soltado")


def padre_vivo():
    if not PADRE:
        return True
    try:
        import ctypes
        k = ctypes.windll.kernel32
        h = k.OpenProcess(0x00100000, False, PADRE)  # SYNCHRONIZE
        if not h:
            return False
        try:
            return k.WaitForSingleObject(h, 0) != 0  # WAIT_OBJECT_0 = terminó
        finally:
            k.CloseHandle(h)
    except Exception:
        return True


def vigilante():
    while True:
        time.sleep(5)
        if not padre_vivo():
            log("la app se cerró: adiós")
            os._exit(0)
        if estado["pipe"] is not None and time.time() - estado["usado"] > SOLTAR_TRAS and estado["ocupado"].acquire(blocking=False):
            try:
                cerrar()
            finally:
                estado["ocupado"].release()


def dilatar(mascara, pasos):
    """La máscara (bool H×W) agrandada `pasos` px (un cuadrado de 3×3 por paso)."""
    import numpy as np
    m = mascara.copy()
    for _ in range(pasos):
        g = m.copy()
        g[1:, :] |= m[:-1, :]
        g[:-1, :] |= m[1:, :]
        g[:, 1:] |= m[:, :-1]
        g[:, :-1] |= m[:, 1:]
        g[1:, 1:] |= m[:-1, :-1]
        g[:-1, :-1] |= m[1:, 1:]
        g[1:, :-1] |= m[:-1, 1:]
        g[:-1, 1:] |= m[1:, :-1]
        m = g
    return m


def membrana(valor, conocido, hueco):
    """
    Un campo suave (alto × ancho × 3): donde `conocido`, el promedio de `valor` en cada celda de una
    rejilla gruesa; dentro de `hueco`, lo que sale de promediar con las vecinas hasta asentarse
    (Laplace, por Jacobi), y de la rejilla a cada píxel, bilineal. Sin bordes ni manchas: lo más
    liso que empalma con lo que se sabe alrededor.
    """
    import numpy as np
    from PIL import Image
    alto, ancho = hueco.shape
    k = max(4, int(round(max(ancho, alto) / 96)))
    gh, gw = -(-alto // k), -(-ancho // k)
    pad = ((0, gh * k - alto), (0, gw * k - ancho))
    a = np.pad(conocido, pad).reshape(gh, k, gw, k)
    hh = np.pad(hueco, pad).reshape(gh, k, gw, k)
    d = np.pad(valor * conocido[..., None], pad + ((0, 0),)).reshape(gh, k, gw, k, 3)
    cuenta = a.sum(axis=(1, 3))
    suma = d.sum(axis=(1, 3))
    sabida = cuenta > 0
    dentro = hh.any(axis=(1, 3)) & ~sabida
    campo = np.zeros((gh, gw, 3), dtype=np.float32)
    campo[sabida] = suma[sabida] / cuenta[sabida][:, None]
    campo[dentro] = campo[sabida].mean(axis=0)
    dominio = sabida | dentro
    # Jacobi: cada celda de adentro, el promedio de sus vecinas del dominio (las sabidas, fijas).
    for _ in range(max(60, 4 * (gh + gw))):
        acc = np.zeros_like(campo)
        n = np.zeros((gh, gw), dtype=np.float32)
        for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            vec = np.roll(campo, (dy, dx), axis=(0, 1))
            ok = np.roll(dominio, (dy, dx), axis=(0, 1))
            if dy == 1:
                ok[0, :] = False
            if dy == -1:
                ok[-1, :] = False
            if dx == 1:
                ok[:, 0] = False
            if dx == -1:
                ok[:, -1] = False
            acc += vec * ok[..., None]
            n += ok
        nuevo = np.where(n[..., None] > 0, acc / np.maximum(n, 1)[..., None], campo)
        campo[dentro] = nuevo[dentro]
    capas = [np.asarray(Image.fromarray(campo[..., c]).resize((gw * k, gh * k), Image.BILINEAR))[:alto, :ancho] for c in range(3)]
    return np.stack(capas, axis=-1)


def corregir_color(salida, original, hueco):
    """
    SDXL Inpainting corre el color de toda la imagen (más de lo que se nota fuera, porque fuera se
    pega la foto): dentro del hueco eso es la costura que se ve. Aquí se mide donde se sabe la verdad
    —en un anillo alrededor del hueco, lo que devolvió el modelo frente a la foto— y se descuenta
    dentro del hueco con una membrana suave: corrige el corrimiento sin tocar lo que el modelo dibujó.
    """
    import numpy as np
    from PIL import Image
    s = np.asarray(salida, dtype=np.float32)
    o = np.asarray(original, dtype=np.float32)
    h = np.asarray(hueco, dtype=bool)
    if not h.any() or h.all():
        return salida
    anillo = dilatar(h, ANILLO) & ~h
    if anillo.sum() < 32:
        return salida
    # acotado: es una corrección, no un repintado
    f = np.clip(membrana(o - s, anillo, h), -48, 48)
    r = np.where(h[..., None], np.clip(s + f, 0, 255), s)
    return Image.fromarray(r.astype(np.uint8), "RGB")


def suavizar(x, pasos=1):
    """Un desenfoque chico (1-2-1 por filas y columnas, `pasos` veces), para el grano del tapado."""
    import numpy as np
    for _ in range(pasos):
        x = (np.roll(x, 1, 0) + 2 * x + np.roll(x, -1, 0)) / 4
        x = (np.roll(x, 1, 1) + 2 * x + np.roll(x, -1, 1)) / 4
    return x


def prellenar(imagen, hueco, semilla=0):
    """
    Para QUITAR (1.29): el hueco tapado con los colores de su borde (una membrana desde un anillo de
    ANILLO_QUITAR px alrededor) y el GRANO de alrededor (ruido con la textura que tiene la foto ahí:
    sin él, el tapado liso parecía un objeto —un disco, una pegatina— y el modelo lo dibujaba como
    tal). Con eso y menos de toda la fuerza, el modelo arranca de la escena sin lo que había y dibuja
    la textura y la luz que siguen; a toda fuerza y desde el ruido, inventaba otra cosa donde estaba
    lo quitado (probado el 15-oct-2026).
    """
    import numpy as np
    from PIL import Image, ImageFilter
    o = np.asarray(imagen, dtype=np.float32)
    h = np.asarray(hueco, dtype=bool)
    if not h.any() or h.all():
        return imagen
    anillo = dilatar(h, ANILLO_QUITAR) & ~h
    if anillo.sum() < 32:
        return imagen
    liso = membrana(o, anillo, h)
    # el grano de alrededor: lo que la foto se aparta de sí misma desenfocada, en un anillo ancho;
    # medido con la mediana (un borde o un objeto cerca no lo infla)
    grano = o - np.asarray(imagen.filter(ImageFilter.GaussianBlur(2)), dtype=np.float32)
    zona = dilatar(h, ANILLO_GRANO) & ~h
    g = grano[zona] if zona.sum() >= 64 else grano[anillo]
    fuerte = 1.4826 * np.median(np.abs(g - np.median(g, axis=0)), axis=0)
    ruido = suavizar(np.random.default_rng(semilla).standard_normal(h.shape).astype(np.float32))
    ruido /= max(1e-6, float(ruido.std()))
    r = np.where(h[..., None], np.clip(liso + ruido[..., None] * fuerte, 0, 255), o)
    return Image.fromarray(r.astype(np.uint8), "RGB")


def una(pipe, semilla, **pedido):
    """Una propuesta, con su semilla; si la tarjeta no da, se limpia y se intenta otra vez."""
    import gc
    import torch
    for intento in (0, 1):
        try:
            g = torch.Generator("cuda").manual_seed(semilla)
            return pipe(generator=g, num_images_per_prompt=1, **pedido).images[0]
        except torch.cuda.OutOfMemoryError:
            gc.collect()
            torch.cuda.empty_cache()
            if intento:
                raise SinMemoria("La tarjeta se quedó sin memoria libre (¿Resolve u otra app la está usando?). Cierra algo o pide menos propuestas.")
            log("sin memoria en la tarjeta: se limpia y se intenta otra vez")


def rellenar(p):
    from PIL import Image
    ancho, alto = int(p["ancho"]), int(p["alto"])
    if not (8 <= ancho <= LADO_MAX and 8 <= alto <= LADO_MAX) or ancho % 8 or alto % 8 or ancho * alto > AREA_MAX:
        raise ValueError(f"El trozo tiene que medir de 8 a {LADO_MAX} px de lado (unos 1,2 MP como mucho), en múltiplos de 8 (llegó {ancho}×{alto}).")
    rgba = base64.b64decode(p["rgba"])
    if len(rgba) != ancho * alto * 4:
        raise ValueError("Los píxeles no cuadran con las medidas.")
    entera = Image.frombytes("RGBA", (ancho, alto), rgba)
    imagen = entera.convert("RGB")
    mascara = entera.getchannel("A").point(lambda a: 255 if a >= 128 else 0)
    n = max(1, min(4, int(p.get("n", 1))))
    pasos = max(10, min(40, int(p.get("pasos", 24))))
    # fuerza: cuánto se aparta de lo que había dentro de lo pintado. Probado el 15-oct-2026: a 0.99
    # (lo de antes) el modelo arranca de la foto con ruido y SIGUE LA ESCENA aunque la frase pida una
    # lámpara; a 1.0 arranca de ruido puro y crea lo pedido (lámpara, cuadro, extintor: 3 de 3).
    # Menos de 1 conserva la forma y la luz de lo que había (y es lo que usa la pasada de detalle de
    # la página). guia: cuánto pesa la frase (12 desde la v3; antes 8, que a veces no bastaba).
    fuerza = max(0.1, min(1.0, float(p.get("fuerza", 1.0))))
    guia = max(1.0, min(15.0, float(p.get("guia", 12.0))))
    semilla = int(p.get("semilla", time.time_ns() % 2_147_483_647))
    frase = str(p.get("frase", "")).strip()
    # sin frase: quitar lo pintado y seguir la escena (la frase lleva el escenario, no un objeto)
    if not frase:
        frase = "empty background, the same scene continued, photorealistic, seamless"
    negativa = str(p.get("negativa", "")).strip() or "blurry, low quality, watermark, text, deformed, extra objects, cartoon"
    import numpy as np
    hueco = np.asarray(mascara) >= 128
    # al quitar, el modelo arranca del hueco tapado con su borde (la fuerza dice cuánto se aparta)
    entrada = prellenar(imagen, hueco, semilla) if p.get("prellenar") is True else imagen
    with estado["ocupado"]:
        pipe = abrir()
        t = time.time()
        # De una en una (no las n de golpe): con Resolve abierto, cuatro a 1 MP juntas no caben.
        salida = [una(pipe, semilla + i, prompt=frase, negative_prompt=negativa, image=entrada, mask_image=mascara, num_inference_steps=pasos, strength=fuerza, guidance_scale=guia, width=ancho, height=alto) for i in range(n)]
        estado["usado"] = time.time()
    imagenes = []
    for im in salida:
        # el color que corrió el modelo, descontado (salvo `color: false`, para comparar); y fuera de
        # lo pintado, byte a byte como llegó
        im = im.convert("RGB")
        if p.get("color", True) is not False:
            im = corregir_color(im, imagen, hueco)
        im = Image.composite(im, imagen, mascara)
        imagenes.append(base64.b64encode(im.tobytes()).decode("ascii"))
    seg = time.time() - t
    log(f"relleno {ancho}×{alto} ×{n} a {pasos} pasos · fuerza {fuerza:.2f} · guía {guia:.1f}{' · prellenado' if entrada is not imagen else ''} en {seg:.1f} s")
    return {"imagenes": imagenes, "ancho": ancho, "alto": alto, "segundos": seg, "semilla": semilla}


class Peticion(BaseHTTPRequestHandler):
    def _json(self, codigo, cuerpo):
        datos = json.dumps(cuerpo).encode("utf-8")
        self.send_response(codigo)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(datos)))
        self.end_headers()
        self.wfile.write(datos)

    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path == "/salud":
            return self._json(200, {"ok": True, "version": VERSION, "modelo": "abierto" if estado["pipe"] is not None else "cerrado", "vram": vram(), "capacidades": CAPACIDADES})
        self._json(404, {"error": "?"})

    def do_POST(self):
        try:
            largo = int(self.headers.get("Content-Length") or 0)
            cuerpo = json.loads(self.rfile.read(largo) or b"{}") if largo else {}
            if self.path == "/rellenar":
                return self._json(200, rellenar(cuerpo))
            if self.path == "/abrir":
                t = time.time()
                with estado["ocupado"]:
                    abrir()
                return self._json(200, {"ok": True, "segundos": time.time() - t})
            if self.path == "/mantener":
                if estado["pipe"] is not None:
                    estado["usado"] = time.time()
                return self._json(200, {"ok": True, "modelo": "abierto" if estado["pipe"] is not None else "cerrado"})
            if self.path == "/cerrar":
                with estado["ocupado"]:
                    cerrar()
                return self._json(200, {"ok": True})
            if self.path == "/salir":
                self._json(200, {"ok": True})
                threading.Thread(target=lambda: (time.sleep(0.2), os._exit(0))).start()
                return
            self._json(404, {"error": "?"})
        except SinMemoria as e:
            log(f"error: {e}")
            self._json(507, {"error": str(e)})
        except Exception as e:  # noqa: BLE001
            log(f"error: {e}")
            self._json(500, {"error": str(e)})


if __name__ == "__main__":
    threading.Thread(target=vigilante, daemon=True).start()
    # Con puerto 0 lo elige el sistema (Windows reserva rangos enteros para Hyper-V).
    servidor = ThreadingHTTPServer(("127.0.0.1", PUERTO), Peticion)
    print(f"puerto {servidor.server_address[1]}", flush=True)
    log(f"motor generativo v{VERSION} en 127.0.0.1:{servidor.server_address[1]} · modelo en {MODELO}")
    servidor.serve_forever()
