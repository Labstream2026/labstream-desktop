# ── El MOTOR GENERATIVO de Labstream OS (15-oct-2026) ──
# Un servidor HTTP en 127.0.0.1 que la app de escritorio arranca en cada PC con tarjeta NVIDIA
# (src/ia/generativo.rs; este archivo va dentro de la app y se copia a la carpeta del motor al
# arrancar, así cambia con la app sin volver a bajar los 4,7 GB del motor). Rellenar con IA le manda
# el trozo de la foto con lo pintado y una frase, y SDXL Inpainting (fp16, en la tarjeta) devuelve
# propuestas. Solo escucha en el equipo; nada sale de él. El modelo se abre la primera vez que se
# pide y se suelta a los SOLTAR_TRAS segundos sin uso (los editores trabajan con Resolve abierto y
# la VRAM es de todos). Si la app que lo arrancó se cierra, se cierra solo.
#
#   GET  /salud            → {ok, version, modelo: "abierto"|"cerrado", vram: {libre, total}}
#   POST /rellenar (JSON)  → {imagenes: [rgb base64 crudo…], ancho, alto, segundos, semilla}
#        {rgba: base64 crudo (ancho × alto × 4; el alfa 255 = rellenar), ancho, alto,
#         frase, negativa?, n?: 1..4, pasos?: 10..40, semilla?, fuerza?: 0..1, guia?: 1..15}
#   POST /cerrar           → suelta el modelo
#   POST /salir            → termina el proceso
#
# Se arranca con: python\python.exe -I servidor.py <puerto (0 = el que haya libre)> <carpeta del
# modelo> [pid de la app]; contesta en la primera línea «puerto <n>».

import base64, io, json, os, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "3"
SOLTAR_TRAS = 180
LADO_MAX = 1024
PUERTO = int(sys.argv[1]) if len(sys.argv) > 1 else 0
MODELO = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "modelo")
PADRE = int(sys.argv[3]) if len(sys.argv) > 3 else 0

estado = {"pipe": None, "usado": 0.0, "ocupado": threading.Lock()}


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
    if estado["pipe"] is None:
        import torch
        from diffusers import StableDiffusionXLInpaintPipeline
        t = time.time()
        pipe = StableDiffusionXLInpaintPipeline.from_pretrained(MODELO, torch_dtype=torch.float16, variant="fp16", use_safetensors=True, local_files_only=True)
        pipe.to("cuda")
        pipe.set_progress_bar_config(disable=True)
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


def rellenar(p):
    import torch
    from PIL import Image
    ancho, alto = int(p["ancho"]), int(p["alto"])
    if not (8 <= ancho <= LADO_MAX and 8 <= alto <= LADO_MAX) or ancho % 8 or alto % 8:
        raise ValueError(f"El trozo tiene que medir de 8 a {LADO_MAX} px de lado, en múltiplos de 8 (llegó {ancho}×{alto}).")
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
    # Menos de 1 conserva la forma y la luz de lo que había. guia: cuánto pesa la frase (12 desde la
    # v3; antes 8, que a veces no bastaba).
    fuerza = max(0.1, min(1.0, float(p.get("fuerza", 1.0))))
    guia = max(1.0, min(15.0, float(p.get("guia", 12.0))))
    semilla = int(p.get("semilla", time.time_ns() % 2_147_483_647))
    frase = str(p.get("frase", "")).strip()
    # sin frase: quitar lo pintado y seguir la escena (la frase lleva el escenario, no un objeto)
    if not frase:
        frase = "empty background, the same scene continued, photorealistic, seamless"
    negativa = str(p.get("negativa", "")).strip() or "blurry, low quality, watermark, text, deformed, extra objects, cartoon"
    with estado["ocupado"]:
        pipe = abrir()
        t = time.time()
        gens = [torch.Generator("cuda").manual_seed(semilla + i) for i in range(n)]
        salida = pipe(prompt=frase, negative_prompt=negativa, image=imagen, mask_image=mascara, num_inference_steps=pasos, strength=fuerza, guidance_scale=guia, num_images_per_prompt=n, generator=gens, width=ancho, height=alto).images
        estado["usado"] = time.time()
    imagenes = []
    for im in salida:
        # fuera de lo pintado, byte a byte como llegó (el modelo solo decide dentro)
        im = Image.composite(im.convert("RGB"), imagen, mascara)
        imagenes.append(base64.b64encode(im.tobytes()).decode("ascii"))
    seg = time.time() - t
    log(f"relleno {ancho}×{alto} ×{n} a {pasos} pasos · fuerza {fuerza:.2f} · guía {guia:.1f} en {seg:.1f} s")
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
            return self._json(200, {"ok": True, "version": VERSION, "modelo": "abierto" if estado["pipe"] is not None else "cerrado", "vram": vram()})
        self._json(404, {"error": "?"})

    def do_POST(self):
        try:
            largo = int(self.headers.get("Content-Length") or 0)
            cuerpo = json.loads(self.rfile.read(largo) or b"{}") if largo else {}
            if self.path == "/rellenar":
                return self._json(200, rellenar(cuerpo))
            if self.path == "/cerrar":
                with estado["ocupado"]:
                    cerrar()
                return self._json(200, {"ok": True})
            if self.path == "/salir":
                self._json(200, {"ok": True})
                threading.Thread(target=lambda: (time.sleep(0.2), os._exit(0))).start()
                return
            self._json(404, {"error": "?"})
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
