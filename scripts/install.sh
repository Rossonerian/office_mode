#!/bin/sh
# Installer for office_mode (supports sudo system-wide install and user-local install)
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Rossonerian/office_mode/main/scripts/install.sh | sudo sh
#   curl -fsSL https://raw.githubusercontent.com/Rossonerian/office_mode/main/scripts/install.sh | sh
# Options:
#   --version <tag>   Install a specific release tag (e.g. v1.0.0)
#   --prefix <dir>    Install into <dir>/bin and <dir>/share/office_mode
#   --uninstall       Remove installed binaries and shared assets
#   -h, --help        Show this help

set -eu

REPO="Rossonerian/office_mode"
VERSION="${VERSION:-}"
PREFIX="${PREFIX:-}"
UNINSTALL=0

usage() {
    cat <<'EOF'
office_mode installer

Usage:
  install.sh [--version <tag>] [--prefix <dir>] [--uninstall]

Defaults:
  When run as root (e.g. via sudo):
    bin dir:   /usr/local/bin
    assets:    /usr/local/share/office_mode/assets
  When run as a regular user:
    bin dir:   ~/.local/bin
    assets:    ~/.local/share/office_mode/assets
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { echo "error: --version requires an argument" >&2; exit 1; }
            VERSION="$2"
            shift 2
            ;;
        --prefix)
            [ "$#" -ge 2 ] || { echo "error: --prefix requires an argument" >&2; exit 1; }
            PREFIX="$2"
            shift 2
            ;;
        --uninstall)
            UNINSTALL=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [ -n "$PREFIX" ]; then
    BIN_DIR="$PREFIX/bin"
    SHARE_DIR="$PREFIX/share/office_mode"
elif [ "$(id -u)" -eq 0 ]; then
    BIN_DIR="/usr/local/bin"
    SHARE_DIR="/usr/local/share/office_mode"
else
    BIN_DIR="${HOME}/.local/bin"
    SHARE_DIR="${HOME}/.local/share/office_mode"
fi

if [ "$UNINSTALL" -eq 1 ]; then
    echo "Removing office_mode from $BIN_DIR and $SHARE_DIR ..."
    rm -f "$BIN_DIR/office" "$BIN_DIR/office-hook" "$BIN_DIR/office_mode" "$BIN_DIR/office-desktop"
    rm -rf "$SHARE_DIR"
    echo "Done. (If you installed agent hooks, run 'office install-hooks --uninstall' before removing, or clean ~/.claude/settings.json and ~/.gemini/config/plugins/agent-office.)"
    exit 0
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) OS_PART="apple-darwin" ;;
    Linux)  OS_PART="unknown-linux-gnu" ;;
    *)
        echo "error: unsupported OS: $OS (use install.ps1 on Windows)" >&2
        exit 1
        ;;
esac

case "$ARCH" in
    arm64|aarch64) ARCH_PART="aarch64" ;;
    x86_64|amd64)  ARCH_PART="x86_64" ;;
    *)
        echo "error: unsupported architecture: $ARCH" >&2
        exit 1
        ;;
esac

TARGET="${ARCH_PART}-${OS_PART}"

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: required command not found: $1" >&2
        exit 1
    }
}

need_cmd curl
need_cmd tar

if [ -z "$VERSION" ]; then
    API_URL="https://api.github.com/repos/${REPO}/releases/latest"
    VERSION="$(curl -fsSL "$API_URL" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    if [ -z "$VERSION" ]; then
        echo "error: could not determine latest release tag from $API_URL" >&2
        exit 1
    fi
fi

ARCHIVE="office_mode-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/office_mode.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM

echo "Downloading ${ARCHIVE} (${VERSION})..."
curl -fsSL -o "${TMP_DIR}/${ARCHIVE}" "${BASE_URL}/${ARCHIVE}"
curl -fsSL -o "${TMP_DIR}/SHA256SUMS.txt" "${BASE_URL}/SHA256SUMS.txt"

EXPECTED_SUM="$(grep " ${ARCHIVE}\$" "${TMP_DIR}/SHA256SUMS.txt" | awk '{print $1}' | head -n 1 || true)"
if [ -n "$EXPECTED_SUM" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL_SUM="$(sha256sum "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL_SUM="$(shasum -a 256 "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
    else
        echo "error: neither sha256sum nor shasum found to verify checksum" >&2
        exit 1
    fi
    if [ "$EXPECTED_SUM" != "$ACTUAL_SUM" ]; then
        echo "error: SHA-256 mismatch for ${ARCHIVE}" >&2
        echo "  expected: ${EXPECTED_SUM}" >&2
        echo "  actual:   ${ACTUAL_SUM}" >&2
        exit 1
    fi
    echo "Checksum verified (${ACTUAL_SUM})."
else
    echo "error: checksum entry for ${ARCHIVE} not found in SHA256SUMS.txt" >&2
    exit 1
fi

UNPACK_DIR="${TMP_DIR}/unpack"
mkdir -p "$UNPACK_DIR"
tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "$UNPACK_DIR"

[ -f "${UNPACK_DIR}/office" ] || { echo "error: archive missing office binary" >&2; exit 1; }
[ -f "${UNPACK_DIR}/office-hook" ] || { echo "error: archive missing office-hook binary" >&2; exit 1; }

mkdir -p "$BIN_DIR" "$SHARE_DIR"
rm -rf "${SHARE_DIR}/assets"
cp -R "${UNPACK_DIR}/assets" "${SHARE_DIR}/assets"

install_bin() {
    src="$1"
    dst="$2"
    if command -v install >/dev/null 2>&1; then
        install -m 755 "$src" "$dst"
    else
        cp "$src" "$dst"
        chmod 755 "$dst"
    fi
}

install_bin "${UNPACK_DIR}/office" "${BIN_DIR}/office"
install_bin "${UNPACK_DIR}/office-hook" "${BIN_DIR}/office-hook"
ln -sf "${BIN_DIR}/office" "${BIN_DIR}/office_mode"
INSTALLED_BINS="${BIN_DIR}/office, ${BIN_DIR}/office-hook, ${BIN_DIR}/office_mode"
if [ -f "${UNPACK_DIR}/office-desktop" ]; then
    install_bin "${UNPACK_DIR}/office-desktop" "${BIN_DIR}/office-desktop"
    INSTALLED_BINS="${INSTALLED_BINS}, ${BIN_DIR}/office-desktop"
fi

echo ""
echo "Installed office_mode (${VERSION})!"
echo "  binaries: ${INSTALLED_BINS}"
echo "  assets:   ${SHARE_DIR}/assets"
echo ""
echo "Next steps:"
echo "  office install-hooks   # wire Claude Code + Antigravity hooks"
echo "  office doctor          # check setup & bundled themes"
echo "  office desktop --demo  # preview native desktop app (or 'office-desktop')"
echo "  office demo            # preview terminal office with synthetic agents"
echo "  office                 # launch live terminal office (or 'office desktop')"

case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        echo ""
        echo "Note: ${BIN_DIR} is not currently in your PATH. Add it to your shell profile:"
        echo "  export PATH=\"${BIN_DIR}:\$PATH\""
        ;;
esac
