# Labstream OS — App de escritorio (Windows + macOS)

Cliente de escritorio para **Labstream OS**, hecho con [Tauri 2](https://tauri.app).
Desde la v1.1.0 no es un envoltorio de una sola vista: es un **shell con pestañas**
(multiwebview) sobre el servidor del NAS (`https://os.labstreamsas.com`).

No contiene la app: el backend, la base de datos, OnlyOffice, etc. siguen en el
NAS. Esto le da al equipo un cliente nativo de verdad en Windows y Mac:

- **Pestañas** como en un navegador: los enlaces internos con `target="_blank"`
  (documentos, entregables, exportaciones, OnlyOffice…) abren **pestaña dentro de
  la app** — antes morían en silencio. También ⌘/Ctrl+clic y clic central.
  Atajos: ⌘/Ctrl+T nueva, ⌘/Ctrl+W cerrar, Ctrl+Tab rotar, ⌘/Ctrl+1…9 ir a la n.
  La sesión (pestañas abiertas, activa) **se restaura** al reabrir.
- **La barra de pestañas ES la fila del título** (v1.6.0), en Windows y en macOS:
  no se gasta una franja entera en repetir el nombre de la app. En Windows la
  ventana va sin decoración y la propia barra dibuja los botones minimizar /
  maximizar / cerrar; en macOS el semáforo lo sigue pintando el sistema y la barra
  le deja su hueco. El doble clic en el hueco libre maximiza y restaura.
- **Colores de pestaña** (v1.6.0): clic derecho sobre una pestaña ▸ **Color**, con
  la misma paleta de nueve tonos que usa Chrome para sus grupos. Sirve para agrupar
  de un vistazo (todo lo de un cliente en verde) y **se guarda con la sesión**.
- **Zoom de interfaz**: ⌘/Ctrl `+` / `−` / `0` y Ctrl+rueda, con indicador en la
  barra y **persistencia** entre sesiones (50%–250%).
- **Documentos dentro de la app**: los visores/editores del propio servidor
  (OnlyOffice, PDFs, reproductor de revisión) abren en pestañas; solo lo de OTRO
  origen (Drive…) sale al **navegador del sistema**. El **login por Authentik**
  sigue funcionando dentro.
- **Notas de voz / cámara**: pide permiso de micrófono y cámara (entitlements de macOS).
- **Descargas** (entregables, exportaciones) con el diálogo nativo de guardado.
- **Bandeja del sistema**: cerrar la ventana la oculta; la app sigue notificando.
  En macOS, el clic en el **Dock** también reabre la ventana, y hay **menú nativo**
  en español (Edición/Vista/Archivo) con los atajos.
- **Arranque automático** al iniciar sesión y **sesión persistente** (no re-loguea).
- **Una sola instancia** (release): abrirla de nuevo enfoca la ventana existente.
- **Menú de opciones ⋮** (v1.2.0) en la barra — también con clic derecho sobre ella:
  pestañas, zoom, recargar, abrir la página en el navegador, **Iniciar con el
  sistema** (elección recordada) y **Buscar actualización…** con aviso del
  resultado. Es un popup nativo del sistema, igual en Windows y macOS, y vive
  solo en el shell (la web app no se toca).

- **Rastreador de trabajo efectivo** (v1.10.0, `src-tauri/src/tracker.rs`): mide
  cuánto tiempo se trabaja de verdad y en qué aplicación, y lo publica en el
  panel **/rastreo** del servidor. Cada 5 s mira la ventana al frente y si
  hubo entrada de ratón/teclado; **con 3 minutos sin entrada deja de contar**
  (eso cubre también pantalla bloqueada y suspensión). Sube lotes cada 5 min a
  `POST /api/tracker`; sin red, la cola espera en disco y reintenta.
  - **Desde la 1.9 también ANOTA la inactividad**: cuando se cruza el umbral de
    3 min, el tramo quieto queda registrado aparte (solo desde/hasta — sin app,
    sin título, sin contenido) y el panel lo enseña junto a cada jornada. La
    bandeja lo dice («inactivo (se anota aparte)»). Suspender o apagar el
    equipo NO cuenta como inactividad: el tramo se corta ahí — la noche con el
    portátil cerrado es una laguna, no un dato (el tramo en curso se guarda a
    disco cada minuto, así que un apagado o la auto-actualización lo cierran en
    su último latido en vez de perderlo). Y un tramo se cierra solo a las 4 h:
    más que eso sin tocar el equipo es ausencia, no descanso frente a la
    pantalla. En pausa no se anota nada.
  - **Desde la 1.10, la excepción del VIDEO** (solo Windows): si no hay entrada
    PERO al frente hay un navegador (Chrome/Brave/Edge/Firefox…) y está saliendo
    sonido —un video reproduciéndose—, ese rato NO se anota como inactividad: se
    cuenta como tiempo de navegador (con 0 segundos «activos», porque no se
    teclea). Se detecta con el medidor de audio del sistema (WASAPI,
    `IAudioMeterInformation`): no abre el navegador, no lee la pestaña, no graba
    nada — solo pregunta si hay sonido saliendo. Límite honesto: un video **mudo**
    no se detecta (no suena). En macOS no aplica (no hay API pública sencilla de
    nivel de audio); el equipo de edición es Windows.
  - **Qué NO registra, a propósito:** ni qué teclas se pulsan (solo compara una
    huella de posición del ratón y cuántas teclas hay presionadas), ni
    pantallazos, ni el contenido de nada, ni los procesos de fondo.
  - **Se ve y se pausa**: el menú de la bandeja dice el estado («registrando»,
    «inactivo», «en pausa») y pausa/reanuda de un toque.
  - **Se vincula** desde el servidor: Ajustes ▸ Perfil ▸ *Vincular este equipo*,
    **estando dentro de esta app**. El servidor genera el token del equipo y se
    lo entrega al sensor por el evento `ls-tracker-token`; el secreto nunca se
    enseña ni se copia a mano. Revocar el equipo (misma pantalla) lo corta al
    instante: el siguiente lote recibe 401, el sensor tira el token y la bandeja
    vuelve a «sin vincular».
  - En **macOS** el sistema pide permiso de *Accesibilidad* la primera vez (para
    leer el título de la ventana al frente); sin él, el sensor mide tiempo pero
    no sabe decir en qué app.

La barra de pestañas vive en `dist/index.html` (webview local «chrome»); las
pestañas son webviews hijos `tab-*`. Shell ↔ Rust se hablan por eventos `ls-*`.

El servidor objetivo se configura en `src-tauri/src/lib.rs` → constante `SERVER_URL`.

### Portadas autenticadas (1.14.1)

Las imagenes conservan su URL HTTPS y se cargan con la sesion del webview, igual
que en el navegador. Ya no se reescriben a `lsthumb://`: el descargador Rust no
comparte las cookies y fallaba con miniaturas sin token o con token vencido.
Tampoco se modifica el DOM de las imagenes durante la hidratacion.

Se usa la cache HTTP con los ETag y Cache-Control del servidor. Una respuesta
304 reutiliza los bytes descargados; no implica fabricar otro fotograma. El
cajon antiguo no se borra, pero no se usa para las nuevas peticiones de imagenes.
No cambia el almacenamiento de originales, la cache HLS de LabTem ni los datos offline.

Google Docs, Sheets, Drive y el inicio de sesion de Google se abren en el navegador
del sistema. Google no admite OAuth en webviews controlados por una app; ademas,
WKWebView no debe anunciarse como Chrome. Se conserva el agente de usuario nativo
y no se borran cookies ni sesiones. Las hojas conectadas siguen disponibles como
tabla dentro del area contable. Al reiniciar se restauran solo pestanas de Labstream.

Pruebas: `npm test`; con Playwright disponible en Node,
`node tests/images.browser.cjs` verifica sesion, 304, carga dinamica y lienzo.
La prueba de navegador usa un servidor local y una cookie ficticia, sin datos reales.

Salidas:

- **Windows** → instalador `.exe` (NSIS).
- **macOS** → `.dmg` universal (funciona en Intel y Apple Silicon).

---

## Prerrequisitos (solo para construir)

Quien instala **no necesita nada** (en Windows, WebView2 ya viene en Win 10/11;
en Mac, el WebView del sistema). Para *construir* hacen falta:

- [Node.js 20+](https://nodejs.org)
- [Rust](https://rustup.rs) (incluye `cargo`)
- En Windows: "Visual Studio Build Tools" con el componente de C++.
- En Mac: Xcode Command Line Tools (`xcode-select --install`).

> ⚠️ **Cada sistema se construye en su sistema:** el `.exe` se compila en Windows
> y el `.dmg` en Mac. Tauri no compila de forma cruzada de manera fiable. Por eso
> lo normal es dejar que **GitHub Actions** construya ambos (ver abajo). Como
> excepción cómoda: si trabajas en Mac, el `.dmg` **sí** lo puedes hacer en local.

---

## Construir los instaladores con GitHub Actions (recomendado)

Genera **ambos** instaladores sin tener las dos máquinas a mano:

1. Sube este proyecto a su propio repositorio (ej. `Labstream2026/labstream-desktop`).
2. Cambia la versión en **dos** sitios (deben coincidir):
   - `src-tauri/tauri.conf.json` → `"version"`
   - `src-tauri/Cargo.toml` → `version`
   - (opcional) `package.json` → `"version"`
3. Crea y publica la etiqueta de versión:
   ```bash
   git commit -am "Versión 1.1.0"
   git tag v1.1.0
   git push origin main --tags
   ```
4. GitHub Actions construye el `.exe` (Windows) y el `.dmg` (macOS) en paralelo y
   los publica juntos en **Releases**:
   `https://github.com/Labstream2026/labstream-desktop/releases`.

También puedes lanzarlo a mano sin etiqueta desde **Actions → Build instaladores
→ Run workflow** (los instaladores quedan como *artifacts*).

---

## Construir en local

### En Mac (tu máquina) → genera el `.dmg`

```bash
npm install
npm run icons
npm run tauri build -- --bundles dmg
```

El `.dmg` queda en:
```
src-tauri/target/release/bundle/dmg/Labstream OS_<versión>_<arch>.dmg
```

> Para un `.dmg` universal (Intel + Apple Silicon) en local:
> ```bash
> rustup target add aarch64-apple-darwin x86_64-apple-darwin
> npm run tauri build -- --target universal-apple-darwin --bundles dmg
> ```

### En una PC Windows → genera el `.exe`

```bash
npm install
npm run tauri build -- --bundles nsis
```
Queda en `src-tauri/target/release/bundle/nsis/Labstream OS_<versión>_x64-setup.exe`.

Para desarrollo con recarga en caliente (en cualquier sistema): `npm run dev`.

---

## Versiones (SemVer)

Se usa `MAYOR.MENOR.PARCHE`:

| Cambio                         | Ejemplo         |
| ------------------------------ | --------------- |
| Corrección / arreglo           | 1.0.0 → 1.0.1   |
| Función nueva compatible       | 1.0.1 → 1.1.0   |
| Cambio grande / incompatible   | 1.1.0 → 2.0.0   |

La versión del **envoltorio** es independiente de la de la app web
(`labstream-os`). Una sola etiqueta de versión genera el instalador de Windows y
el de Mac a la vez, así ambas plataformas van siempre sincronizadas.

---

## Actualización automática (ACTIVA)

Las apps ya instaladas **se actualizan solas**: el updater consulta el `latest.json`
que publica cada Release, y los instaladores se firman en el CI con la clave privada
guardada como secret del repo (`TAURI_SIGNING_PRIVATE_KEY`). También se puede forzar
la comprobación con clic en la versión de la barra, o desde el menú ⋮ ▸ *Buscar
actualización…*.

> ⚠️ De ahí se sigue algo que conviene tener presente: **un commit en `main` no le
> llega a nadie**. Mientras no exista la etiqueta `vX.Y.Z`, el CI no construye nada y
> el updater no ve versión nueva. Y al revés: publicar una etiqueta **actualiza la app
> de todo el equipo**, así que no es un paso que se dé a la ligera.

## Probar un cambio antes de publicarlo

`.github/workflows/check.yml` corre en cualquier rama que no sea `main` y **no publica
nada**:

- `cargo check` en Windows y macOS — comprueba que compila en las dos.
- Un job aparte deja el **instalador `.exe` de prueba** como *artifact* de la ejecución
  (14 días), para instalarlo encima y ver el cambio funcionando de verdad.

Es la forma de mirar un cambio visual —la barra, los colores— sin sacárselo al equipo.

---

## Firma de macOS

### Lo que pasaba hasta la 1.15.0

El build **no firmaba** la app de macOS. No se le pasaba ninguna identidad, así que
el bundler de Tauri se saltaba `codesign` entero y el `.dmg` salía con la firma ad hoc
que deja el enlazador. En la app instalada se veía así:

```
$ codesign -dv "/Applications/Labstream OS.app"
Identifier=labstream_desktop-0a5819ba37119701
CodeDirectory ... flags=0x20002(adhoc,linker-signed)
Info.plist=not bound
Sealed Resources=none
TeamIdentifier=not set
```

Tres cosas mal, y ninguna avisaba:

- **`Entitlements.plist` no se aplicaba.** Los permisos de micrófono, cámara y JIT
  eran decoración: se escriben al firmar, y no se firmaba.
- **`Info.plist=not bound` y `Sealed Resources=none`.** Ni el Info.plist ni los
  recursos iban dentro de la firma: cualquiera podía cambiarlos sin romperla.
- **Identificador de enlazador** (`labstream_desktop-<hash>`) en vez del de la app
  (`co.labstream.os`), que es por lo que macOS recuerda los permisos concedidos.

### Lo que hace ahora (desde la 1.16.0)

`build.yml` decide según los secrets que haya en el repo:

| Secrets | Resultado |
| --- | --- |
| ninguno | Firma **ad hoc** (`-s -`) con hardened runtime y los entitlements. Gatekeeper sigue avisando, pero el bundle va sellado y los permisos se aplican. |
| `APPLE_CERTIFICATE` + `APPLE_CERTIFICATE_PASSWORD` | Firma real con **Developer ID**. Gatekeeper deja de avisar en cualquier Mac. |
| …y además `APPLE_ID` + `APPLE_PASSWORD` + `APPLE_TEAM_ID` | Firma real + **notarizado y grapado**. Ni el primer arranque avisa. |

Y hay un paso que **tumba la publicación** si los entitlements no llegaron al `.app`.
Ese paso es la lección: que esto estuviera roto no lo dijo nada durante meses.

### Pasar a Developer ID (99 USD/año)

1. Alta en el Apple Developer Program.
2. En *Certificates* crear uno de tipo **Developer ID Application** y exportarlo del
   Llavero como `.p12` con contraseña.
3. Convertirlo a texto: `base64 -i certificado.p12 | pbcopy`.
4. En GitHub → *Settings ▸ Secrets and variables ▸ Actions*, añadir:
   - `APPLE_CERTIFICATE` — lo que quedó en el portapapeles.
   - `APPLE_CERTIFICATE_PASSWORD` — la contraseña del `.p12`.
   - `APPLE_SIGNING_IDENTITY` — opcional; por defecto `Developer ID Application`.
5. Para notarizar, además:
   - `APPLE_ID` — el correo de la cuenta de desarrollador.
   - `APPLE_PASSWORD` — una **contraseña específica de app** (appleid.apple.com, no la
     del Apple ID).
   - `APPLE_TEAM_ID` — el identificador de equipo de 10 caracteres.

No hay que tocar ni un archivo: en cuanto los secrets existan, la siguiente etiqueta
sale firmada.

### Mientras siga ad hoc

- **macOS (Gatekeeper):** *"no se puede abrir porque proviene de un desarrollador no
  identificado"* → clic derecho sobre la app → **Abrir** → **Abrir**. (Si insiste:
  `xattr -cr "/Applications/Labstream OS.app"`.)
- **Windows (SmartScreen):** *"Windows protegió su PC"* → **Más información →
  Ejecutar de todos modos**. Windows es aparte: su firma cuesta 150–400 USD/año y no
  la toca nada de lo de arriba.

---

## Estructura

```
labstream-desktop/
├── app-icon.png                 fuente del ícono (1024×1024)
├── dist/index.html              pantalla de respaldo (requerida por Tauri)
├── package.json                 scripts de build
├── src-tauri/
│   ├── tauri.conf.json          nombre, versión, iconos, instaladores, entitlements
│   ├── Cargo.toml               dependencias Rust + versión
│   ├── Info.plist               textos de permiso (micrófono/cámara) en macOS
│   ├── Entitlements.plist       entitlements de macOS (micrófono, cámara, JIT)
│   ├── build.rs
│   ├── capabilities/default.json
│   ├── icons/                   se generan desde app-icon.png
│   └── src/{main.rs,lib.rs}     lib.rs crea la ventana + SERVER_URL + tray
└── .github/workflows/build.yml  build de .exe (Windows) y .dmg (macOS) + firma de macOS
```
