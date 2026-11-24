#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
ID=io.VagueMotion.TokenBurnTrack
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
IMAGE=${TBT_APPIMAGE_IMAGE:-docker.io/library/ubuntu:22.04}
TOOLS=${XDG_CACHE_HOME:-$HOME/.cache}/tokenburntrack/appimage
APPDIR=$ROOT/target/appimage/AppDir
DIST=$ROOT/dist

if [ "${1:-}" = --host ]; then
  scripts/cargo build --release --locked -p tbt-app -p tbt-daemon
  BIN=target/release
else
  podman run --rm --security-opt label=disable \
    -v "$ROOT":/src -w /src \
    -v tbt-appimage-cargo:/root/.cargo -v tbt-appimage-rustup:/root/.rustup \
    -e CARGO_TARGET_DIR=/src/target/appimage \
    "$IMAGE" bash -euc '
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq --no-install-recommends ca-certificates curl gcc g++ clang cmake \
        pkg-config libxkbcommon-dev libwayland-dev libxcb1-dev libx11-dev \
        libfontconfig1-dev libfreetype-dev >/dev/null
      [ -x /root/.cargo/bin/rustup ] ||
        curl -sSf https://sh.rustup.rs | sh -s -- -y -q --default-toolchain none --profile minimal
      /root/.cargo/bin/cargo build --release --locked -p tbt-app -p tbt-daemon'
  BIN=target/appimage/release
fi

TOOL=$TOOLS/appimagetool-$ARCH.AppImage
if [ ! -x "$TOOL" ]; then
  mkdir -p "$TOOLS"
  curl -fL -o "$TOOL.part" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
  chmod +x "$TOOL.part"
  mv "$TOOL.part" "$TOOL"
fi

rm -rf "$APPDIR"
install -Dm755 -t "$APPDIR/usr/bin" "$BIN/tbt-app"
install -Dm644 -t "$APPDIR/usr/share/applications" "packaging/linux/$ID.desktop"
install -Dm644 -t "$APPDIR/usr/share/icons/hicolor/scalable/apps" "packaging/icons/$ID.svg"
install -Dm644 -t "$APPDIR/usr/share/metainfo" "packaging/flatpak/$ID.metainfo.xml"
ln -s "usr/share/applications/$ID.desktop" "$APPDIR/$ID.desktop"
ln -s "usr/share/icons/hicolor/scalable/apps/$ID.svg" "$APPDIR/$ID.svg"
ln -s "$ID.svg" "$APPDIR/.DirIcon"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
exec "$(dirname "$(readlink -f "$0")")/usr/bin/tbt-app" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

mkdir -p "$DIST"
OUT=$DIST/TokenBurnTrack-$VERSION-$ARCH.AppImage
ARCH=$ARCH APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT"
install -m755 "$BIN/tbt-daemon" "$DIST/tbt-daemon-$VERSION-linux-$ARCH"
echo "$OUT"
echo "$DIST/tbt-daemon-$VERSION-linux-$ARCH"
