# Prueba vistas_red.rs contra una COPIA de las vistas del Explorador, sin tocar las de verdad.
# 1. Respalda la rama real (o usa -Respaldo, un .reg ya hecho con reg export de esa rama).
# 2. La copia a HKCU\Software\Labstream-prueba-vistas\Shell.
# 3. Corre la prueba ignorada `con_la_copia_del_registro` (pone el ajuste y olvida las vistas de red de
#    vídeo/fotos/música EN LA COPIA, y comprueba que una segunda vuelta no cambia nada).
# 4. Borra la copia.
param([string]$Respaldo)
# reg.exe y cargo escriben en stderr aunque todo vaya bien: se mira $LASTEXITCODE, no los errores.
$ErrorActionPreference = 'Continue'
$real = 'HKEY_CURRENT_USER\Software\Classes\Local Settings\Software\Microsoft\Windows\Shell'
$copia = 'HKEY_CURRENT_USER\Software\Labstream-prueba-vistas\Shell'
$tmp = Join-Path $env:TEMP "vistas-red-$PID"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  if (-not $Respaldo) {
    $Respaldo = Join-Path $tmp 'real.reg'
    & reg.exe export $real $Respaldo /y | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'No se pudo exportar la rama real.' }
  }
  $texto = [IO.File]::ReadAllText($Respaldo, [Text.Encoding]::Unicode)
  $texto = $texto.Replace("[$real", "[$copia")
  if ($texto.Contains("[$real")) { throw 'La copia todavía apunta a la rama real.' }
  $reg = Join-Path $tmp 'copia.reg'
  [IO.File]::WriteAllText($reg, $texto, [Text.Encoding]::Unicode)
  cmd /c "reg import `"$reg`" 2>nul"
  if ($LASTEXITCODE -ne 0) { throw 'No se pudo importar la copia.' }
  Push-Location (Join-Path $PSScriptRoot '..\src-tauri')
  try { cmd /c "cargo test --lib vistas_red -- --ignored --nocapture 2>&1" } finally { Pop-Location }
  if ($LASTEXITCODE -ne 0) { throw 'La prueba falló.' }
} finally {
  cmd /c "reg delete HKCU\Software\Labstream-prueba-vistas /f >nul 2>nul"
  Remove-Item -Recurse -Force $tmp
}
