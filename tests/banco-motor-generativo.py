# Banco de pruebas del MOTOR GENERATIVO (src-tauri/src/ia/servidor.py) en la tarjeta de este PC
# (15-oct-2026, para la 1.29). Arranca el motor de este repo con el modelo que la app ya bajó y
# deja hojas de comparación (JPEG) en LS_SALIDA:
#   ciclo    abrir, rellenar, mantener, soltar a los 20 s sin uso, volver a abrir (los tiempos)
#   calidad  los mismos huecos como la 1.28 (trozo a su tamaño, sin corregir el color) y como la
#            1.29 (trozo a ~1 MP, color corregido, pasada de detalle si se generó más chico)
#   quitar / quitar2 / demo   quitar sin frase: desde el ruido frente al hueco tapado (con grano),
#            a varias fuerzas; `demo`, con pinceles como los pintaría un editor
# Se corre con el Python del motor (trae torch, diffusers, numpy y PIL), desde la raíz del repo:
#   & "$env:LOCALAPPDATA\co.labstream.os\ia\motor-generativo\python\python.exe" -I tests\banco-motor-generativo.py demo
# LS_FOTOS: la carpeta de las fotos de los casos (CASOS / CASOS_DEMO); LS_MODELO, LS_SALIDA.
import base64, json, math, os, subprocess, sys, tempfile, threading, time, urllib.error, urllib.request

import numpy as np

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
from PIL import Image, ImageDraw

PY = sys.executable
SERVIDOR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src-tauri", "src", "ia", "servidor.py")
MODELO = os.environ.get("LS_MODELO") or os.path.join(os.environ["LOCALAPPDATA"], "co.labstream.os", "ia", "sdxl-inpaint")
FOTOS = os.environ.get("LS_FOTOS") or r"C:\Users\Munka\OneDrive\Labstream\02_Danney\Descripción procedimientos\Imagenes Adicionales"
SALIDA = os.environ.get("LS_SALIDA") or os.path.join(tempfile.gettempdir(), "banco-motor-generativo")
os.makedirs(SALIDA, exist_ok=True)


def get(base, ruta):
    return json.loads(urllib.request.urlopen(base + ruta, timeout=10).read())


def post(base, ruta, cuerpo=None, timeout=1200):
    req = urllib.request.Request(base + ruta, data=json.dumps(cuerpo or {}).encode(), headers={"Content-Type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"{e.code}: {e.read()[:300]!r}")


def arrancar(env_extra):
    env = dict(os.environ, PYTHONIOENCODING="utf-8", **env_extra)
    p = subprocess.Popen([PY, "-I", SERVIDOR, "0", MODELO], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, text=True, encoding="utf-8", errors="replace")
    linea = p.stdout.readline()
    puerto = int(linea.split()[1])
    threading.Thread(target=lambda: [print("    motor:", l.rstrip(), flush=True) for l in p.stdout], daemon=True).start()
    base = f"http://127.0.0.1:{puerto}"
    for _ in range(300):
        try:
            get(base, "/salud")
            break
        except Exception:
            time.sleep(0.3)
    return p, base


def rellenar(base, rgb, hueco, frase="", n=1, fuerza=1.0, guia=None, pasos=24, semilla=1234, color=True, prellenar=False):
    h, w = hueco.shape
    rgba = np.dstack([rgb, (hueco * 255).astype(np.uint8)])
    cuerpo = {"rgba": base64.b64encode(np.ascontiguousarray(rgba).tobytes()).decode(), "ancho": w, "alto": h, "frase": frase, "n": n, "fuerza": fuerza, "guia": guia if guia is not None else (12 if frase else 7), "pasos": pasos, "semilla": semilla, "color": color}
    if prellenar:
        cuerpo["prellenar"] = True
    r = post(base, "/rellenar", cuerpo)
    return [np.frombuffer(base64.b64decode(im), dtype=np.uint8).reshape(h, w, 3) for im in r["imagenes"]], r["segundos"]


# ── 1. El ciclo ──
def ciclo():
    print("== 1. El ciclo del modelo (se suelta a los 20 s sin uso)", flush=True)
    p, base = arrancar({"LS_SOLTAR_TRAS": "20"})
    try:
        s = get(base, "/salud")
        print(f"  salud: v{s['version']} · modelo {s['modelo']} · capacidades {s['capacidades']}", flush=True)
        t = time.time()
        post(base, "/abrir")
        print(f"  abrir (del disco): {time.time() - t:.1f} s", flush=True)
        rgb = np.full((768, 1024, 3), 128, np.uint8)
        hueco = np.zeros((768, 1024), bool)
        hueco[300:460, 400:620] = True
        _, seg = rellenar(base, rgb, hueco, n=2)
        print(f"  2 propuestas a 1024×768, de una en una: {seg:.1f} s", flush=True)
        print(f"  mantener: {post(base, '/mantener')}", flush=True)
        time.sleep(29)
        print(f"  a los 29 s sin uso: {get(base, '/salud')['modelo']}", flush=True)
        t = time.time()
        post(base, "/abrir")
        print(f"  abrir otra vez (del disco, ya en la caché de Windows): {time.time() - t:.1f} s", flush=True)
    finally:
        try:
            post(base, "/salir", timeout=5)
        except Exception:
            pass
        p.wait(timeout=20)


# ── 2. La calidad ──
FORMA = [2.0]


def pincel(ancho, alto, cx, cy, rx, ry, difuminar=0.25):
    """El pincel del editor: elipse (o, con FORMA 4, casi un rectángulo) con el borde suave (0..1)."""
    y, x = np.mgrid[0:alto, 0:ancho].astype(np.float32)
    e = FORMA[0]
    d = (np.abs((x + 0.5 - cx) / rx) ** e + np.abs((y + 0.5 - cy) / ry) ** e) ** (1 / e)
    duro = 1 - difuminar
    t = np.clip((d - duro) / max(1e-6, 1 - duro), 0, 1)
    return np.where(d >= 1, 0, 1 - t * t * (3 - 2 * t)).astype(np.float32)


def caja(W, H, cx, cy, rx, ry, minimo=0):
    """La caja del trozo: lo pintado con aire (0,6 de su lado mayor a cada lado) y, si `minimo`, al menos eso de lado."""
    lado = 2 * max(rx, ry)
    a = lado * 0.6
    x0, x1, y0, y1 = cx - rx - a, cx + rx + a, cy - ry - a, cy + ry + a
    if minimo:
        if x1 - x0 < minimo:
            m = (minimo - (x1 - x0)) / 2
            x0, x1 = x0 - m, x1 + m
        if y1 - y0 < minimo:
            m = (minimo - (y1 - y0)) / 2
            y0, y1 = y0 - m, y1 + m
    w, h = min(W, x1 - x0), min(H, y1 - y0)
    x0 = min(max(0, x0), W - w)
    y0 = min(max(0, y0), H - h)
    return int(round(x0)), int(round(y0)), int(round(w)), int(round(h))


def tam_modelo(w, h, lado_max=1536, area=1024 * 1024, subir_max=4.0):
    s = min(subir_max, math.sqrt(area / (w * h)))
    W, H = w * s, h * s
    k = min(1.0, lado_max / max(W, H))
    W, H = W * k, H * k
    return max(64, int(W // 8) * 8), max(64, int(H // 8) * 8)


def viejo(base, foto, cx, cy, rx, ry, frase, semilla):
    """Como la 1.28: el trozo a su tamaño real (si cabe en 1024), sin corregir el color."""
    W, H = foto.size
    x0, y0, w, h = caja(W, H, cx, cy, rx, ry)
    w, h = max(64, round(w / 8) * 8), max(64, round(h / 8) * 8)
    x0, y0 = min(max(0, int(cx - w / 2)), W - w), min(max(0, int(cy - h / 2)), H - h)
    trozo = np.asarray(foto.crop((x0, y0, x0 + w, y0 + h)).convert("RGB"))
    alfa = pincel(w, h, cx - x0, cy - y0, rx, ry)
    if w <= 1024 and h <= 1024:
        (sal,), seg = rellenar(base, trozo, alfa > 0.02, frase=frase, semilla=semilla, color=False)
    else:
        # de la foto del editor (lado mayor 2560), a lo más 1024, y estirado de vuelta
        k = min(1024 / max(w, h), 2560 / max(W, H))
        mw, mh = max(8, int(w * k) // 8 * 8), max(8, int(h * k) // 8 * 8)
        chico = np.asarray(Image.fromarray(trozo).resize((mw, mh), Image.LANCZOS))
        (sal,), seg = rellenar(base, chico, np.asarray(Image.fromarray(alfa).resize((mw, mh), Image.BILINEAR)) > 0.02, frase=frase, semilla=semilla, color=False)
        sal = np.asarray(Image.fromarray(sal).resize((w, h), Image.BILINEAR))
    final = trozo * (1 - alfa[..., None]) + sal * alfa[..., None]
    return (x0, y0, w, h), final.astype(np.uint8), seg, (w, h)


def firme(alfa):
    """El borde suave del pincel, en su mitad de fuera: dentro, todo lo generado."""
    t = np.clip(alfa / 0.5, 0, 1)
    return t * t * (3 - 2 * t)


def nuevo(base, foto, cx, cy, rx, ry, frase, semilla, fuerza=1.0, prellenar=False, solo_generado=False):
    """1.29: trozo con al menos 512 px de contexto, a ~1 MP para el modelo, color corregido y, si se
    generó más chico que el trozo, una pasada de detalle a tamaño real."""
    W, H = foto.size
    x0, y0, w, h = caja(W, H, cx, cy, rx, ry, minimo=512)
    trozo = np.asarray(foto.crop((x0, y0, x0 + w, y0 + h)).convert("RGB"))
    alfa = pincel(w, h, cx - x0, cy - y0, rx, ry)
    hueco = alfa > 0.02
    mw, mh = tam_modelo(w, h)
    sube = mw * mh >= w * h
    entrada = np.asarray(Image.fromarray(trozo).resize((mw, mh), Image.BICUBIC if sube else Image.LANCZOS))
    hm = np.asarray(Image.fromarray(alfa).resize((mw, mh), Image.BILINEAR)) > 0.02
    (sal,), seg = rellenar(base, entrada, hm, frase=frase, semilla=semilla, fuerza=fuerza, prellenar=prellenar)
    if sube:
        img = np.where(hueco[..., None], np.asarray(Image.fromarray(sal).resize((w, h), Image.LANCZOS)), trozo)
    else:
        img = np.where(hueco[..., None], np.asarray(Image.fromarray(sal).resize((w, h), Image.BICUBIC)), trozo)
        img, s2 = afinar(base, img, hueco, frase, semilla)
        seg += s2
    if solo_generado:
        return (x0, y0, w, h), (trozo, img, alfa), seg, (mw, mh)
    final = trozo * (1 - alfa[..., None]) + img * alfa[..., None]
    return (x0, y0, w, h), final.astype(np.uint8), seg, (mw, mh)


def afinar(base, img, hueco, frase, semilla, lado=1024, solape=128):
    """La pasada de detalle: cuadros de hasta 1024 a tamaño real sobre el hueco, con poca fuerza."""
    h, w = hueco.shape
    ys, xs = np.nonzero(hueco)
    bx0, bx1 = max(0, xs.min() - 64), min(w, xs.max() + 65)
    by0, by1 = max(0, ys.min() - 64), min(h, ys.max() + 65)
    tw, th = min(lado, w // 8 * 8), min(lado, h // 8 * 8)
    def posiciones(a, b, t, total):
        if b - a <= t:
            c = min(max(0, (a + b) // 2 - t // 2), total - t)
            return [c]
        n = math.ceil((b - a - solape) / (t - solape))
        paso = (b - a - t) / (n - 1)
        return [min(total - t, int(round(a + i * paso))) for i in range(n)]
    acc = np.zeros((h, w, 3), np.float32)
    peso = np.zeros((h, w), np.float32)
    seg = 0.0
    i = 0
    for ty in posiciones(by0, by1, th, h):
        for tx in posiciones(bx0, bx1, tw, w):
            hq = hueco[ty:ty + th, tx:tx + tw]
            if not hq.any():
                continue
            (sal,), s = rellenar(base, img[ty:ty + th, tx:tx + tw], hq, frase=frase, fuerza=0.35, guia=7 if frase else 5, pasos=40, semilla=semilla + 100 + i)
            seg += s
            i += 1
            yy = np.minimum(np.arange(th), np.arange(th)[::-1]).astype(np.float32)
            xx = np.minimum(np.arange(tw), np.arange(tw)[::-1]).astype(np.float32)
            m = np.minimum(np.clip((yy[:, None] + 1) / 64, 0, 1), np.clip((xx[None, :] + 1) / 64, 0, 1))
            acc[ty:ty + th, tx:tx + tw] += sal * m[..., None]
            peso[ty:ty + th, tx:tx + tw] += m
    r = np.where((peso > 0)[..., None], acc / np.maximum(peso, 1e-6)[..., None], img)
    return np.where(hueco[..., None], r, img).astype(np.uint8), seg


CASOS = [
    # foto, centro y radios en px de la foto completa, frase
    ("beautician-making-mesotherapy-treatment-with-derma-2022-05-03-23-16-41-utc.jpg", "llave del mueble (hueco chico)", 5532, 1890, 85, 140, ""),
    ("beautician-making-mesotherapy-treatment-with-derma-2022-05-03-23-16-41-utc.jpg", "caja de la encimera (hueco mediano)", 5067, 890, 520, 300, ""),
    ("close-up-of-pipette-with-fluid-collagen-and-hyalur-2022-08-01-01-28-47-utc.jpg", "gota sobre el fondo rosa", 2727, 1101, 410, 410, ""),
    ("close-up-of-pipette-with-fluid-collagen-and-hyalur-2022-08-01-01-28-47-utc.jpg", "crear: un anillo dorado", 1254, 2563, 380, 380, "a small gold ring lying on the pink paper, product photo"),
]


def calidad():
    print("== 2. La calidad: la 1.28 frente a la 1.29", flush=True)
    p, base = arrancar({})
    try:
        post(base, "/abrir")
        for i, (f, que, cx, cy, rx, ry, frase) in enumerate(CASOS):
            foto = Image.open(os.path.join(FOTOS, f))
            (vx, vy, vw, vh), v, sv, mv = viejo(base, foto, cx, cy, rx, ry, frase, 777)
            (nx, ny, nw, nh), n, sn, mn = nuevo(base, foto, cx, cy, rx, ry, frase, 777)
            print(f"  {i + 1}. {que}: la 1.28 vio {mv[0]}×{mv[1]} ({sv:.1f} s) · 1.29 vio {mn[0]}×{mn[1]} ({sn:.1f} s)", flush=True)
            # la misma zona para comparar: la caja de hoy (más chica), en la foto completa
            ox, oy, ow, oh = vx, vy, vw, vh
            antes = np.asarray(foto.crop((ox, oy, ox + ow, oy + oh)).convert("RGB"))
            hoy = v
            nueva = np.asarray(foto.convert("RGB"))[oy:oy + oh, ox:ox + ow].copy()
            # pegar lo nuevo en su sitio y recortar la misma zona
            lienzo = np.asarray(foto.convert("RGB")).copy()
            lienzo[ny:ny + nh, nx:nx + nw] = n
            nueva = lienzo[oy:oy + oh, ox:ox + ow]
            alto = 560
            tiras = [Image.fromarray(a).resize((int(a.shape[1] * alto / a.shape[0]), alto), Image.LANCZOS) for a in (antes, hoy, nueva)]
            hoja = Image.new("RGB", (sum(t.width for t in tiras) + 40, alto + 44), (24, 24, 24))
            d = ImageDraw.Draw(hoja)
            x = 10
            for t, rotulo in zip(tiras, ("Original", "1.28", "1.29")):
                hoja.paste(t, (x, 34))
                d.text((x + 4, 8), rotulo, fill=(235, 235, 235))
                x += t.width + 10
            hoja.save(os.path.join(SALIDA, f"comparacion-{i + 1}.jpg"), quality=90)
    finally:
        try:
            post(base, "/salir", timeout=5)
        except Exception:
            pass
        p.wait(timeout=20)


def hoja(columnas, ruta, alto=420):
    tiras = [(Image.fromarray(a).resize((int(a.shape[1] * alto / a.shape[0]), alto), Image.LANCZOS), r) for a, r in columnas]
    lienzo = Image.new("RGB", (sum(t.width for t, _ in tiras) + 10 * (len(tiras) + 1), alto + 44), (24, 24, 24))
    d = ImageDraw.Draw(lienzo)
    x = 10
    for t, rotulo in tiras:
        lienzo.paste(t, (x, 34))
        d.text((x + 4, 8), rotulo, fill=(235, 235, 235))
        x += t.width + 10
    lienzo.save(ruta, quality=90)


def quitar():
    """Quitar sin frase: la 1.29 tal cual (toda la fuerza, desde el ruido) frente al hueco tapado
    con su borde y menos fuerza; y el borde del pincel firme (lo generado tapa más)."""
    print("== 3. Quitar: desde el ruido frente a tapado con el borde", flush=True)
    p, base = arrancar({})
    try:
        post(base, "/abrir")
        for i, (f, que, cx, cy, rx, ry, frase) in enumerate(CASOS):
            if frase:
                continue
            foto = Image.open(os.path.join(FOTOS, f))
            cols = []
            caja_ref = None
            for rotulo, fz, pre in (("1.29 (100 %, desde el ruido)", 1.0, False), ("Tapado + 85 %", 0.85, True), ("Tapado + 70 %", 0.7, True)):
                (x0, y0, w, h), (trozo, img, alfa), seg, (mw, mh) = nuevo(base, foto, cx, cy, rx, ry, frase, 777, fuerza=fz, prellenar=pre, solo_generado=True)
                print(f"  {i + 1}. {que} · {rotulo}: {mw}×{mh}, {seg:.1f} s", flush=True)
                if caja_ref is None:
                    caja_ref = (x0, y0, w, h)
                    cols.append((trozo, "Original"))
                cols.append(((trozo * (1 - alfa[..., None]) + img * alfa[..., None]).astype(np.uint8), rotulo))
                if pre and fz == 0.85:
                    a2 = firme(alfa)
                    cols.append(((trozo * (1 - a2[..., None]) + img * a2[..., None]).astype(np.uint8), "Tapado + 85 % · borde firme"))
            hoja(cols, os.path.join(SALIDA, f"quitar-{i + 1}.jpg"))
    finally:
        try:
            post(base, "/salir", timeout=5)
        except Exception:
            pass
        p.wait(timeout=20)


def quitar2():
    """Quitar con el tapado con grano, a tres fuerzas."""
    print("== 4. Quitar: tapado con grano", flush=True)
    p, base = arrancar({})
    try:
        post(base, "/abrir")
        for i, (f, que, cx, cy, rx, ry, frase) in enumerate(CASOS):
            if frase:
                continue
            foto = Image.open(os.path.join(FOTOS, f))
            cols = []
            for fz in (0.7, 0.8, 0.9):
                (x0, y0, w, h), (trozo, img, alfa), seg, (mw, mh) = nuevo(base, foto, cx, cy, rx, ry, frase, 777, fuerza=fz, prellenar=True, solo_generado=True)
                print(f"  {i + 1}. {que} · grano {round(fz * 100)} %: {mw}×{mh}, {seg:.1f} s", flush=True)
                if not cols:
                    cols.append((trozo, "Original"))
                cols.append(((trozo * (1 - alfa[..., None]) + img * alfa[..., None]).astype(np.uint8), f"Tapado con grano + {round(fz * 100)} %"))
            hoja(cols, os.path.join(SALIDA, f"quitar2-{i + 1}.jpg"))
    finally:
        try:
            post(base, "/salir", timeout=5)
        except Exception:
            pass
        p.wait(timeout=20)


CASOS_DEMO = [
    # como lo pintaría un editor: el objeto entero, con un poco de margen
    ("beautician-making-mesotherapy-treatment-with-derma-2022-05-03-23-16-41-utc.jpg", "llave del mueble, con el llavero", 5532, 2010, 90, 270, 2.0),
    ("beautician-making-mesotherapy-treatment-with-derma-2022-05-03-23-16-41-utc.jpg", "caja de la encimera, entera", 5067, 890, 640, 370, 4.0),
    ("close-up-of-pipette-with-fluid-collagen-and-hyalur-2022-08-01-01-28-47-utc.jpg", "gota sobre el papel rosa, con su borde", 2727, 1101, 480, 480, 2.0),
]


def demo():
    """Quitar, la 1.28 frente a la 1.29 (tapado con grano, 75 %), con pinceles de verdad."""
    print("== 5. Quitar: la 1.28 frente a la 1.29", flush=True)
    p, base = arrancar({})
    try:
        post(base, "/abrir")
        for i, (f, que, cx, cy, rx, ry, forma) in enumerate(CASOS_DEMO):
            FORMA[0] = forma
            foto = Image.open(os.path.join(FOTOS, f))
            (vx, vy, vw, vh), v, sv, mv = viejo(base, foto, cx, cy, rx, ry, "", 777)
            (nx, ny, nw, nh), n, sn, mn = nuevo(base, foto, cx, cy, rx, ry, "", 777, fuerza=0.75, prellenar=True)
            print(f"  {i + 1}. {que}: 1.28 {mv[0]}×{mv[1]} ({sv:.1f} s) · 1.29 {mn[0]}×{mn[1]} ({sn:.1f} s)", flush=True)
            lienzo_v = np.asarray(foto.convert("RGB")).copy()
            lienzo_v[vy:vy + vh, vx:vx + vw] = v
            lienzo_n = np.asarray(foto.convert("RGB")).copy()
            lienzo_n[ny:ny + nh, nx:nx + nw] = n
            # la misma zona en las tres: la caja de la 1.29 (la más grande)
            ox, oy, ow, oh = nx, ny, nw, nh
            orig = np.asarray(foto.convert("RGB"))[oy:oy + oh, ox:ox + ow]
            hoja([(orig, "Original"), (lienzo_v[oy:oy + oh, ox:ox + ow], "1.28"), (lienzo_n[oy:oy + oh, ox:ox + ow], "1.29")], os.path.join(SALIDA, f"demo-quitar-{i + 1}.jpg"), alto=520)
        FORMA[0] = 2.0
    finally:
        try:
            post(base, "/salir", timeout=5)
        except Exception:
            pass
        p.wait(timeout=20)


if __name__ == "__main__":
    que = sys.argv[1] if len(sys.argv) > 1 else "todo"
    if que in ("ciclo", "todo"):
        ciclo()
    if que in ("calidad", "todo"):
        calidad()
    if que in ("quitar", "todo"):
        quitar()
    if que == "quitar2":
        quitar2()
    if que == "demo":
        demo()
