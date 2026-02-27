target := arch() + "-apple-darwin"
app    := "tty.app"

build:
    cargo build

release:
    cargo clean -p tty --release --target {{ target }}
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

# build icon.icns from icon.png (1024x1024 source)
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

# install to /Applications
install: release icon
    install -d /Applications/{{ app }}/Contents/MacOS
    install -d /Applications/{{ app }}/Contents/Resources
    install -m 644 Info.plist /Applications/{{ app }}/Contents/
    install -m 644 icon.icns /Applications/{{ app }}/Contents/Resources/
    install -m 755 target/{{ target }}/release/tty /Applications/{{ app }}/Contents/MacOS/
    codesign --force --sign - /Applications/{{ app }}
    @echo "Installed to /Applications/{{ app }}"

run *ARGS:
    cargo run -- {{ ARGS }}

run-release *ARGS:
    cargo run --release -- {{ ARGS }}

stats *ARGS:
    cargo run -- --stats {{ ARGS }}

setup:
  prek install --install-hooks

pc:
  prek run --all-files
