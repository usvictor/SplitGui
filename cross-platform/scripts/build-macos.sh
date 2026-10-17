#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
rustup target add aarch64-apple-darwin x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release --locked --target aarch64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release --locked --target x86_64-apple-darwin
app="package/TailscaleSplitGUI.app"
mkdir -p "$app/Contents/MacOS" dist
lipo -create target/aarch64-apple-darwin/release/tailscale-splitgui target/x86_64-apple-darwin/release/tailscale-splitgui -output "$app/Contents/MacOS/TailscaleSplitGUI"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.aibalabala.tailscalesplit.gui.v2</string>
<key>CFBundleName</key><string>TailscaleSplitGUI</string>
<key>CFBundleExecutable</key><string>TailscaleSplitGUI</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>2.0.0</string>
<key>CFBundleVersion</key><string>20001</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
ditto -c -k --sequesterRsrc --keepParent "$app" dist/TailscaleSplitGUI-v2-macOS-universal.zip
cp README.md dist/README.md
