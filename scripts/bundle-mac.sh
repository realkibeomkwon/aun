#!/bin/sh
# Builds target/release/Aun.app and target/release/Aun-mac.zip.
# Signs with MAC_SIGN_IDENTITY (a keychain identity) when set, else ad-hoc.
# Run from anywhere on a Mac. The binary is for this Mac's architecture only.
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p aun
version=$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -n 1)

app=target/release/Aun.app
rm -rf "$app" target/release/Aun-mac.zip
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/aun "$app/Contents/MacOS/aun"
cp assets/Aun.icns "$app/Contents/Resources/Aun.icns"

# No LSUIElement: the app sets its own activation policy from the "Dock 아이콘" setting.
# NSLocalNetworkUsageDescription: a bundled app needs it to reach the LAN on macOS 15 and later.
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key><string>dev.aun.desk</string>
	<key>CFBundleName</key><string>Aun</string>
	<key>CFBundleDisplayName</key><string>Aun</string>
	<key>CFBundleExecutable</key><string>aun</string>
	<key>CFBundleIconFile</key><string>Aun</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleShortVersionString</key><string>$version</string>
	<key>CFBundleVersion</key><string>$version</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSLocalNetworkUsageDescription</key><string>같은 네트워크의 기기를 찾고 연결하는 데 필요해요.</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

# No Developer ID, so Gatekeeper asks once on first open either way.
# A fixed identity makes the designated requirement "identifier + certificate leaf",
# so macOS privacy grants survive updates; ad-hoc pins the cdhash, which every build changes.
codesign --force --sign "${MAC_SIGN_IDENTITY:--}" "$app"
codesign --verify --strict "$app"

ditto -c -k --norsrc --noextattr --keepParent "$app" target/release/Aun-mac.zip
echo "built $app and target/release/Aun-mac.zip ($version)"
