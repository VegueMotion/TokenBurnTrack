#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
ID=io.VagueMotion.TokenBurnTrack
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
APP=target/dist/TokenBurnTrack.app
WORK=target/dist/work
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-11.0}

rustup target add "${TARGETS[@]}"
for t in "${TARGETS[@]}"; do
  cargo build --release --locked --target "$t" -p tbt-app -p tbt-daemon
done

rm -rf "$APP" "$WORK"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$WORK/AppIcon.iconset"
for b in tbt-app tbt-daemon; do
  lipo -create -output "$APP/Contents/MacOS/$b" \
    "target/${TARGETS[0]}/release/$b" "target/${TARGETS[1]}/release/$b"
done

qlmanage -t -s 1024 -o "$WORK" "packaging/icons/$ID.svg" >/dev/null
for s in 16 32 128 256 512; do
  sips -z $s $s "$WORK/$ID.svg.png" --out "$WORK/AppIcon.iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$WORK/$ID.svg.png" --out "$WORK/AppIcon.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns -o "$APP/Contents/Resources/AppIcon.icns" "$WORK/AppIcon.iconset"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$ID</string>
  <key>CFBundleName</key><string>TokenBurnTrack</string>
  <key>CFBundleDisplayName</key><string>TokenBurnTrack</string>
  <key>CFBundleExecutable</key><string>tbt-app</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
  codesign --force --deep --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP"
fi

mkdir -p dist
DMG=dist/TokenBurnTrack-$VERSION-macos-universal.dmg
rm -f "$DMG"
ln -s /Applications "$WORK/Applications"
cp -R "$APP" "$WORK/"
rm -rf "$WORK/AppIcon.iconset" "$WORK/$ID.svg.png"
hdiutil create -volname TokenBurnTrack -srcfolder "$WORK" -ov -format UDZO "$DMG" >/dev/null
echo "$DMG"
