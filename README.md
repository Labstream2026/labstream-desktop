# Labstream OS — App de escritorio (Windows)

Envoltorio de escritorio para **Labstream OS**. Es un cliente delgado hecho con
[Tauri 2](https://tauri.app): abre una ventana propia (sin barra de navegador)
que carga el servidor de Labstream OS que corre en el NAS
(`https://os.labstreamsas.com`).

No contiene la app: el backend, la base de datos, OnlyOffice, etc. siguen en el
NAS. Esto solo le da al equipo un ícono y una ventana propia en Windows.

El servidor objetivo se configura en `src-tauri/tauri.conf.json` →
`app.windows[0].url`.

---

## Prerrequisitos (solo para construir)

Quien instala el `.exe` **no necesita nada** (WebView2 ya viene en Windows 10/11).
Para *construir* el instalador hacen falta:

- [Node.js 20+](https://nodejs.org)
- [Rust](https://rustup.rs) (incluye `cargo`)
- En Windows: las "Visual Studio Build Tools" con el componente de C++.

> ⚠️ **Importante:** el `.exe` de Windows se construye **en Windows**. Tauri no
> compila de forma cruzada desde macOS de manera fiable. Por eso lo normal es
> dejar que **GitHub Actions** lo construya (ver más abajo). Si quieres hacerlo
> a mano, usa una PC o máquina virtual con Windows.

---

## Construir el instalador con GitHub Actions (recomendado)

El flujo no requiere tener Windows a mano:

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
4. GitHub Actions construye el `.exe` y lo publica en **Releases**. El equipo lo
   descarga desde ahí: `https://github.com/Labstream2026/labstream-desktop/releases`.

También puedes lanzarlo a mano sin etiqueta desde la pestaña **Actions →
Build instalador Windows → Run workflow** (el `.exe` queda como *artifact*).

---

## Construir en local (en una PC Windows)

```bash
npm install
npm run build      # genera iconos + compila el instalador
```

El instalador queda en:
```
src-tauri/target/release/bundle/nsis/Labstream OS_<versión>_x64-setup.exe
```

Para desarrollo con recarga en caliente: `npm run dev`.

---

## Versiones (SemVer)

Se usa `MAYOR.MENOR.PARCHE`:

| Cambio                         | Ejemplo         |
| ------------------------------ | --------------- |
| Corrección / arreglo           | 1.0.0 → 1.0.1   |
| Función nueva compatible       | 1.0.1 → 1.1.0   |
| Cambio grande / incompatible   | 1.1.0 → 2.0.0   |

La versión del **envoltorio** es independiente de la de la app web
(`labstream-os`). Como la ventana siempre apunta al mismo servidor, casi nunca
chocan; documenta aquí si en algún momento una versión del envoltorio exige una
versión mínima del backend.

---

## Actualización automática (opcional, recomendable a futuro)

Hoy, para actualizar, el equipo descarga el nuevo `.exe` del Release y lo
reinstala encima. Para que las apps ya instaladas se actualicen solas, Tauri
trae un *updater*. Para activarlo (paso futuro):

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

## Aviso de SmartScreen (instalación interna)

Como el instalador **no está firmado** con un certificado de pago, Windows
mostrará una pantalla azul *"Windows protegió su PC"* la primera vez. Es normal
para apps internas:

> **Más información → Ejecutar de todos modos**

Si en el futuro se distribuye a clientes externos, conviene un certificado de
*code signing* (~150–400 USD/año) para quitar ese aviso.

---

## Estructura

```
labstream-desktop/
├── app-icon.png                 fuente del ícono (1024×1024)
├── dist/index.html              pantalla de respaldo (requerida por Tauri)
├── package.json                 scripts de build
├── src-tauri/
│   ├── tauri.conf.json          nombre, versión, URL del servidor, instalador
│   ├── Cargo.toml               dependencias Rust + versión
│   ├── build.rs
│   ├── capabilities/default.json
│   ├── icons/                   se generan desde app-icon.png
│   └── src/{main.rs,lib.rs}
└── .github/workflows/build-windows.yml   build automático del .exe
```
