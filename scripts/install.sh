#!/bin/sh
# Instalador de AstroDev para Linux y macOS.
#   curl -fsSL https://github.com/AstroSoftwareMoon/AstroCode/releases/latest/download/install.sh | sh
# Variables opcionales:
#   ASTRODEV_VERSION=v0.1.0      versión concreta (por defecto la última)
#   ASTRODEV_INSTALL_DIR=~/bin   destino (por defecto ~/.local/bin)
#   ASTRODEV_DOWNLOAD_URL=…      espejo con los archivos de la release
set -eu

REPO="AstroSoftwareMoon/AstroCode"
DIR="${ASTRODEV_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${ASTRODEV_VERSION:-latest}"

say() { printf '\033[1;35m✦\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "hace falta '$1'"; }

need uname
need tar
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "hace falta curl o wget"
fi

os=$(uname -s)
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) die "arquitectura no soportada: $arch" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-gnu" ;;
  Darwin) target="$arch-apple-darwin" ;;
  *) die "sistema no soportado: $os (en Windows usa install.ps1)" ;;
esac

if [ -n "${ASTRODEV_DOWNLOAD_URL:-}" ]; then
  base="$ASTRODEV_DOWNLOAD_URL"   # espejo propio
elif [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi
file="astrodev-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "Descargando $file ($VERSION)…"
fetch "$base/$file" "$tmp/$file" || die "no se pudo descargar $base/$file"
fetch "$base/$file.sha256" "$tmp/$file.sha256" || die "no se pudo descargar la suma SHA-256"

say "Verificando SHA-256…"
expected=$(cut -d' ' -f1 < "$tmp/$file.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$file" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/$file" | cut -d' ' -f1)
fi
[ "$expected" = "$actual" ] || die "el hash no coincide (esperado $expected, obtenido $actual)"

tar -xzf "$tmp/$file" -C "$tmp"
mkdir -p "$DIR"
install -m 755 "$tmp/astrodev-$target/astrodev" "$DIR/astrodev" 2>/dev/null || {
  cp "$tmp/astrodev-$target/astrodev" "$DIR/astrodev"
  chmod 755 "$DIR/astrodev"
}
# macOS: quitar la cuarentena de Gatekeeper del binario descargado.
if [ "$os" = "Darwin" ]; then xattr -d com.apple.quarantine "$DIR/astrodev" 2>/dev/null || true; fi

say "AstroDev instalado en $DIR/astrodev"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *)
    shell_rc="$HOME/.profile"
    case "${SHELL:-}" in */zsh) shell_rc="$HOME/.zshrc" ;; */bash) shell_rc="$HOME/.bashrc" ;; esac
    printf '\n  %s no está en tu PATH. Añádelo con:\n    echo '\''export PATH="%s:$PATH"'\'' >> %s && . %s\n' "$DIR" "$DIR" "$shell_rc" "$shell_rc"
    ;;
esac
say "Ejecuta 'astrodev' dentro de un proyecto para empezar."
