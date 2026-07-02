# Labstream OS — App de escritorio (Windows + macOS)

Envoltorio de escritorio para **Labstream OS**. Es un cliente delgado hecho con
[Tauri 2](https://tauri.app): abre una ventana propia (sin barra de navegador)
que carga el servidor de Labstream OS que corre en el NAS
(`https://os.labstreamsas.com`).

No contiene la app: el backend, la base de datos, OnlyOffice, etc. siguen en el
NAS. Esto solo le da al equipo un ícono y una ventana propia en Windows y Mac.

Además del ícono y la ventana, el envoltorio añade comportamiento de cliente nativo:

- **Enlaces externos** (Drive, documentos ajenos…) abren en el **navegador del sistema**;
  la navegación del propio servidor y el **login por Authentik** se quedan dentro.
- **Notas de voz / cámara**: pide permiso de micrófono y cámara (entitlements de macOS).
- **Descargas** (entregables, exportaciones) con el diálogo nativo de guardado.
- **Bandeja del sistema**: cerrar la ventana la oculta; la app sigue notificando.
- **Arranque automático** al iniciar sesión y **sesión persistente** (no re-loguea).

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

## Actualización automática (opcional, recomendable a futuro)

Hoy, para actualizar, el equipo descarga el nuevo instalador del Release y lo
reinstala encima. Para que las apps ya instaladas se actualicen solas, Tauri trae
un *updater* (funciona igual en Windows y Mac). Para activarlo (paso futuro):

1. Genera el par de claves de firma:
   ```bash
   npm run tauri signer generate -- -w ~/.tauri/labstream.key
   ```
2. Añade el `plugin-updater` y la clave pública en `tauri.conf.json`, y guarda la
   clave privada como **secret** en GitHub (`TAURI_SIGNING_PRIVATE_KEY`).
3. Apunta el updater al `latest.json` que publica el Release.

Se deja sin activar a propósito para que la **primera** versión compile sin
configuración extra.

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
