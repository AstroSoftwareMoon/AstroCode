# Instalador de AstroDev para Windows.
#   irm https://github.com/AstroSoftwareMoon/AstroCode/releases/latest/download/install.ps1 | iex
# Variables opcionales: $env:ASTRODEV_VERSION = "v0.1.0"; $env:ASTRODEV_INSTALL_DIR = "C:\herramientas"
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repo = "AstroSoftwareMoon/AstroCode"
$version = if ($env:ASTRODEV_VERSION) { $env:ASTRODEV_VERSION } else { "latest" }
$dir = if ($env:ASTRODEV_INSTALL_DIR) { $env:ASTRODEV_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "astrodev\bin" }

# En Windows ARM64 se usa la build x86_64 (Windows la ejecuta con emulación).
$target = "x86_64-pc-windows-msvc"
$file = "astrodev-$target.zip"
$base = if ($env:ASTRODEV_DOWNLOAD_URL) { $env:ASTRODEV_DOWNLOAD_URL } elseif ($version -eq "latest") { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("astrodev-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    Write-Host "✦ Descargando $file ($version)..." -ForegroundColor Magenta
    Invoke-WebRequest "$base/$file" -OutFile "$tmp\$file" -UseBasicParsing
    Invoke-WebRequest "$base/$file.sha256" -OutFile "$tmp\$file.sha256" -UseBasicParsing

    Write-Host "✦ Verificando SHA-256..." -ForegroundColor Magenta
    $expected = ((Get-Content "$tmp\$file.sha256" -Raw).Trim() -split "\s+")[0].ToLower()
    $actual = (Get-FileHash "$tmp\$file" -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "El hash no coincide (esperado $expected, obtenido $actual)" }

    Expand-Archive "$tmp\$file" -DestinationPath "$tmp\x" -Force
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item "$tmp\x\astrodev.exe" (Join-Path $dir "astrodev.exe") -Force

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($userPath -split ";" | Where-Object { $_ -eq $dir })) {
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
        Write-Host "  Añadido $dir a tu PATH (abre una terminal nueva)."
    }
    Write-Host "✦ AstroDev instalado en $dir\astrodev.exe" -ForegroundColor Magenta
    Write-Host "  Ejecuta 'astrodev' dentro de un proyecto para empezar."
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
