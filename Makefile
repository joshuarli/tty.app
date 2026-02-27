install:
	rustup component add rust-src --toolchain nightly-2026-02-23-aarch64-apple-darwin

	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo +nightly-2026-02-23 build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target aarch64-apple-darwin

	install -d /Applications/tty.app/Contents/MacOS
	install -m 644 Info.plist /Applications/tty.app/Contents/
	install -m 755 target/aarch64-apple-darwin/release/tty /Applications/tty.app/Contents/MacOS/
	codesign --force --sign - /Applications/tty.app
