NAME       := tty
APP        := tty.app
ARCH       := $(shell uname -m | sed 's/arm64/aarch64/')
TARGET     := $(ARCH)-apple-darwin
LLVM_BIN   := $(shell rustc --print sysroot)/lib/rustlib/$(TARGET)/bin

.PHONY: setup build release-bin pgo-profile release-pgo bench-pgo release install run run-release stats test test-ci pc bump-version

setup:
	rustup show active-toolchain
	prek install --install-hooks

build:
	cargo build

release-bin:
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release --bin $(NAME) \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

PGO_DIR    := $(CURDIR)/target/pgo-profiles
PGO_MERGED := $(PGO_DIR)/merged.profdata

pgo-profile:
	rm -rf $(PGO_DIR)
	mkdir -p $(PGO_DIR)
	RUSTFLAGS="-Cprofile-generate=$(PGO_DIR)" \
	cargo bench --bench bench -- --profile-time 1 "parser|pipeline|end_to_end"
	$(LLVM_BIN)/llvm-profdata merge -o $(PGO_MERGED) $(PGO_DIR)

release-pgo: $(PGO_MERGED)
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort -Cprofile-use=$(PGO_MERGED)" \
	cargo build --release --bin $(NAME) \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
	@echo "==> PGO release binary: target/$(TARGET)/release/$(NAME)"

# Benchmark regular release vs PGO. Requires: critcmp (cargo install critcmp)
bench-pgo: $(PGO_MERGED)
	cargo bench --bench bench -- --save-baseline regular 2>/dev/null
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	cargo bench --bench bench -- --save-baseline pgo 2>/dev/null
	critcmp regular pgo

$(PGO_MERGED):
	$(MAKE) pgo-profile

icon.icns: icon.png
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

release: release-pgo icon.icns
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources
	cp Info.plist $(APP)/Contents/
	cp icon.icns $(APP)/Contents/Resources/
	cp target/$(TARGET)/release/$(NAME) $(APP)/Contents/MacOS/
	zip -r $(APP).zip $(APP)

install: release
	unzip -o $(APP).zip -d /Applications
	codesign --force --sign - /Applications/$(APP)
	@echo "Installed to /Applications/$(APP)"

run:
	cargo run

run-release:
	cargo run --release

stats:
	cargo run -- --stats

test:
	@OUT=$$(cargo test --quiet -- --test-threads=32 2>&1) || { echo "$$OUT"; exit 1; }

# So we don't do duplicate work (building both debug and release) in CI.
test-ci:
	@OUT=$$(cargo test --quiet --release -- --test-threads=32 2>&1) || { echo "$$OUT"; exit 1; }

pc:
	prek run --quiet --all-files

# Usage: make bump-version [V=x.y.z]
# Without V, increments the patch version.
bump-version:
ifndef V
	$(eval OLD := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml))
	$(eval V := $(shell echo "$(OLD)" | awk -F. '{printf "%d.%d.%d", $$1, $$2, $$3+1}'))
endif
	sed -i '' 's/^version = ".*"/version = "$(V)"/' Cargo.toml
	cargo check --quiet 2>/dev/null
	git add Cargo.toml Cargo.lock
	git commit -m "bump version to $(V)"
	git tag "release/$(V)"
