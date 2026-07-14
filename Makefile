NAME       := tty
APP        := tty.app
ARCH       := $(shell uname -m | sed 's/arm64/aarch64/')
TARGET     := $(ARCH)-apple-darwin
LLVM_BIN   := $(shell rustc --print sysroot)/lib/rustlib/$(TARGET)/bin

.PHONY: build prof pgo-profile dist-pgo bench-pgo dist install test test-ci coverage pc bump-version

# inspect with:
# heap <pid>
# vmmap --summary <pid>
# malloc_history <pid> <address>
debug:
	cargo build
	MallocStackLoggingNoCompact=1 \
	MallocScribble=1 \
	./target/debug/tty

lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --workspace --all-targets -- --deny warnings

PGO_DIR    := $(CURDIR)/target/pgo-profiles
PGO_MERGED := $(PGO_DIR)/merged.profdata
PROF_SCRIPT := $(CURDIR)/scripts/profile.sh
PROFILE_BENCH_ARGS := --noplot --sample-size 10 --warm-up-time 0.2 --measurement-time 0.5
PROFILE_RUN_DIR := $(CURDIR)/target/profiling/latest

prof:
	$(MAKE) pgo-profile
	$(MAKE) dist-pgo
	$(MAKE) bench-pgo

pgo-profile:
	PROFILE_BENCH_ARGS='$(PROFILE_BENCH_ARGS)' $(PROF_SCRIPT)

dist-pgo: $(PGO_MERGED)
	cargo clean -p $(NAME) --profile dist --target $(TARGET)
	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort -Cprofile-use=$(PGO_MERGED)" \
	cargo build --profile dist --bin $(NAME) \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)
	@echo "==> PGO dist binary: target/$(TARGET)/dist/$(NAME)"

# Benchmark dist vs PGO dist. Requires: critcmp (cargo install critcmp)
bench-pgo: $(PGO_MERGED)
	mkdir -p $(PROFILE_RUN_DIR)
	cargo bench --bench bench --profile dist -- $(PROFILE_BENCH_ARGS) --save-baseline regular 2>$(PROFILE_RUN_DIR)/bench-pgo-regular.log
	RUSTFLAGS="-Cprofile-use=$(PGO_MERGED)" \
	cargo bench --bench bench --profile dist -- $(PROFILE_BENCH_ARGS) --save-baseline pgo 2>$(PROFILE_RUN_DIR)/bench-pgo-pgo.log
	@grep -h '^[[:space:]]*\[rss\]' $(PROFILE_RUN_DIR)/bench-pgo-regular.log $(PROFILE_RUN_DIR)/bench-pgo-pgo.log || true
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

dist: dist-pgo icon.icns
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources
	cp Info.plist $(APP)/Contents/
	cp icon.icns $(APP)/Contents/Resources/
	cp target/$(TARGET)/dist/$(NAME) $(APP)/Contents/MacOS/
	zip -r $(APP).zip $(APP)

install: dist
	unzip -o $(APP).zip -d /Applications
	codesign --force --sign - /Applications/$(APP)
	@echo "Installed to /Applications/$(APP)"

test:
	cargo test --quiet

test-ci:
	cargo test --profile dist

coverage:
	cargo llvm-cov --all-targets --ignore-run-fail --lcov --output-path lcov.info

coverage-report:
	cargo llvm-cov --all-targets --ignore-run-fail --open

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
