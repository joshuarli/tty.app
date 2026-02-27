target := arch() + "-apple-darwin"

# dev build (debug)
build:
    cargo build

# optimized stable release (fat LTO, stripped)
release:
    cargo build --release

# maximum optimization: nightly build-std + immediate-abort panics
release-nightly:
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

# run dev build
run *ARGS:
    cargo run -- {{ ARGS }}

# run release build
run-release *ARGS:
    cargo run --release -- {{ ARGS }}

# run nightly release build
run-nightly *ARGS:
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly run --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }} -- {{ ARGS }}

# show binary sizes for all release builds
sizes: release release-nightly
    @printf "stable:  " && ls -lh target/release/etch | awk '{print $5}'
    @printf "nightly: " && ls -lh target/{{ target }}/release/etch | awk '{print $5}'

# remove build artifacts
clean:
    cargo clean
