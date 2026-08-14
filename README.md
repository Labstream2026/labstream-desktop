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

- **Rastreador de trabajo efectivo** (v1.8.0, `src-tauri/src/tracker.rs`): mide
  cuánto tiempo se trabaja de verdad y en qué aplicación, y lo publica en
  **Reportes → Equipo** del servidor. Cada 5 s mira la ventana al frente y si
  hubo entrada de ratón/teclado; **con 3 minutos sin entrada deja de contar**
  (eso cubre también pantalla bloqueada y suspensión). Sube lotes cada 5 min a
  `POST /api/tracker`; sin red, la cola espera en disco y reintenta.
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

## Avisos de seguridad del sistema (instalación interna sin firmar)

Como los instaladores **no están firmados** con certificado de pago, la primera
vez cada sistema muestra una advertencia. Es normal para apps internas:

- **Windows (SmartScreen):** pantalla azul *"Windows protegió su PC"* →
  **Más información → Ejecutar de todos modos**.
- **macOS (Gatekeeper):** *"no se puede abrir porque proviene de un desarrollador
  no identificado"* → clic derecho sobre la app → **Abrir** → **Abrir**. (Si
  insiste, ejecutar una vez: `xattr -cr "/Applications/Labstream OS.app"`.)

Si en el futuro se distribuye a clientes externos, conviene firmar:
*code signing* en Windows (~150–400 USD/año) y un *Apple Developer ID* + notarizado
en Mac (99 USD/año) para quitar estos avisos.

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
│   ├── Entitlements.plist       entitlements de macOS (audio-input, cámara, red)
│   ├── build.rs
│   ├── capabilities/default.json
│   ├── icons/                   se generan desde app-icon.png
│   └── src/{main.rs,lib.rs}     lib.rs crea la ventana + SERVER_URL + tray
└── .github/workflows/build.yml  build automático de .exe (Windows) y .dmg (macOS)
```
