target := arch() + "-apple-darwin"
app    := "tty.app"

build:
    cargo build

release:
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

# install to /Applications
install: release
    install -d /Applications/{{ app }}/Contents/MacOS
    install -m 644 Info.plist /Applications/{{ app }}/Contents/
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
