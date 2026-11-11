# Mide cuánto tarda el Explorador en sacar los iconos de una carpeta del NAS (11-nov-2026).
# Uso (PowerShell COMO ADMINISTRADOR: Windows no deja tocar las directivas sin serlo):
#   powershell -ExecutionPolicy Bypass -File medir-carpetas-os.ps1 "\\192.168.0.22\LABSTREAM"
# Hace 3 pasadas con la caché de desktop.ini APAGADA y 3 ENCENDIDA (reinicia el Explorador entre
# medio para que lea la directiva) y deja la caché encendida al terminar.
param([Parameter(Mandatory = $true)][string]$Ruta)

Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Sh {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct SHFILEINFO { public IntPtr hIcon; public int iIcon; public uint attr;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string name;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)] public string type; }
  [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
  public static extern IntPtr SHGetFileInfo(string p, uint a, ref SHFILEINFO i, uint s, uint f);
  public static string Icono(string p) { var i = new SHFILEINFO();
    SHGetFileInfo(p, 0, ref i, (uint)Marshal.SizeOf(i), 0x1000); return i.name; }
}
"@

$clave = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Policies\Explorer'

function Pasada {
  # En frío: Windows guarda la lista de una carpeta de red 10 s; se espera a que caduque.
  Start-Sleep 15
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $carpetas = Get-ChildItem -LiteralPath $Ruta -Directory -Force -ErrorAction SilentlyContinue
  $pintadas = 0
  foreach ($c in $carpetas) { if (([Sh]::Icono($c.FullName)) -like '*carpetas-os*') { $pintadas++ } }
  $sw.Stop()
  [pscustomobject]@{ ms = $sw.ElapsedMilliseconds; carpetas = $carpetas.Count; pintadas = $pintadas }
}

function Explorador {
  Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
  Start-Sleep 3
  if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }
  Start-Sleep 3
}

$ErrorActionPreference = 'Stop'
New-Item -Path $clave -Force | Out-Null
$ErrorActionPreference = 'Continue'
foreach ($valor in 0, 1) {
  Set-ItemProperty -Path $clave -Name UseDesktopIniCache -Value $valor -Type DWord
  Explorador
  1..3 | ForEach-Object {
    $r = Pasada
    "{0}  pasada {1}: {2} ms  ({3} carpetas, {4} con icono del OS)" -f ($(if ($valor) { 'CON cache' } else { 'SIN cache' })), $_, $r.ms, $r.carpetas, $r.pintadas
  }
}
