install: icon
	rustup component add rust-src --toolchain nightly-2026-02-23-aarch64-apple-darwin

	cargo clean -p tty --release --target aarch64-apple-darwin

	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo +nightly-2026-02-23 build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target aarch64-apple-darwin

	install -d /Applications/tty.app/Contents/MacOS
	install -d /Applications/tty.app/Contents/Resources
	install -m 644 Info.plist /Applications/tty.app/Contents/
	install -m 644 icon.icns /Applications/tty.app/Contents/Resources/
	install -m 755 target/aarch64-apple-darwin/release/tty /Applications/tty.app/Contents/MacOS/
	codesign --force --sign - /Applications/tty.app

icon:
	mkdir -p icon.iconset
	sips -z 16 16     icon.png --out icon.iconset/icon_16x16.png
	sips -z 32 32     icon.png --out icon.iconset/icon_16x16@2x.png
	sips -z 32 32     icon.png --out icon.iconset/icon_32x32.png
	sips -z 64 64     icon.png --out icon.iconset/icon_32x32@2x.png
	sips -z 128 128   icon.png --out icon.iconset/icon_128x128.png
	sips -z 256 256   icon.png --out icon.iconset/icon_128x128@2x.png
	sips -z 256 256   icon.png --out icon.iconset/icon_256x256.png
	sips -z 512 512   icon.png --out icon.iconset/icon_256x256@2x.png
	sips -z 512 512   icon.png --out icon.iconset/icon_512x512.png
	sips -z 1024 1024 icon.png --out icon.iconset/icon_512x512@2x.png
	iconutil -c icns icon.iconset -o icon.icns
	rm -rf icon.iconset
