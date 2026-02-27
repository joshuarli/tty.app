target := arch() + "-apple-darwin"
app    := "Etch.app"

build:
    cargo build

release:
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

# assemble Etch.app bundle (run after `just release`)
app: release
    rm -rf {{ app }}
    mkdir -p {{ app }}/Contents/MacOS
    cp Info.plist {{ app }}/Contents/
    cp target/{{ target }}/release/etch {{ app }}/Contents/MacOS/
    @echo "Built {{ app }} ($(du -sh {{ app }} | awk '{print $1}'))"

# install to /Applications
install: app
    rm -rf /Applications/{{ app }}
    cp -r {{ app }} /Applications/
    @echo "Installed to /Applications/{{ app }}"

run *ARGS:
    cargo run -- {{ ARGS }}

run-release *ARGS:
    cargo run --release -- {{ ARGS }}
