#!/bin/sh
# Build Shitty.app and package it as a .zip and a .dmg with SHA-256 sums.
#
#   tools/package-macos.sh <version> [out-dir]
#
# <version> goes into Info.plist and the file names, e.g. 0.1.0 or
# 0.1.0-nightly.20261102+ae49c32. The out-dir defaults to dist/.
# Set SKIP_BUILD=1 to package an existing target_local/release/shitty.
set -eu

version="${1:?usage: tools/package-macos.sh <version> [out-dir]}"
out="${2:-dist}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

case "$(uname -m)" in
  arm64) arch=arm64 ;;
  x86_64) arch=x86_64 ;;
  *) echo "package-macos: unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

# Info.plist versions must be numeric (major.minor.patch); keep the full string
# for display and file names.
short_version="${version%%[-+]*}"
# File names avoid '+', which some download tools mangle.
file_version="$(printf %s "$version" | tr '+' '.')"
name="Shitty-$file_version-macos-$arch"

if [ "${SKIP_BUILD:-0}" != 1 ]; then
  cargo build --release --locked
fi
bin="target_local/release/shitty"
[ -x "$bin" ] || { echo "package-macos: $bin not found" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
app="$stage/Shitty.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/assets"

cp "$bin" "$app/Contents/MacOS/shitty"
cp assets/*.ttf assets/icon.png "$app/Contents/Resources/assets/"

# App icon: icon.png -> AppIcon.icns.
iconset="$stage/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Shitty</string>
  <key>CFBundleDisplayName</key><string>Shitty</string>
  <key>CFBundleIdentifier</key><string>com.lixiao189.shitty</string>
  <key>CFBundleExecutable</key><string>shitty</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$short_version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature: Apple Silicon refuses to run unsigned code. This is not
# notarization; Gatekeeper still asks on first open (right-click > Open).
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
rm -f "$out/$name.zip" "$out/$name.dmg"

ditto -c -k --keepParent "$app" "$out/$name.zip"

dmg_src="$stage/dmg"
mkdir -p "$dmg_src"
cp -R "$app" "$dmg_src/"
ln -s /Applications "$dmg_src/Applications"
hdiutil create -quiet -volname "Shitty $version" -srcfolder "$dmg_src" \
  -fs HFS+ -format UDZO "$out/$name.dmg"

(cd "$out" && for f in "$name.zip" "$name.dmg"; do shasum -a 256 "$f" >"$f.sha256"; done)

echo "$out/$name.zip"
echo "$out/$name.dmg"
