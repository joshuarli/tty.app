# tty.app

standard issue terminal emulator

- Apple Silicon only — Rust + Metal compute shader
- ~200 KB binary, zero runtime dependencies
- 8-byte Cell is the GPU format � dirty rows memcpy'd directly to Metal buffer
- Ring buffer grid — O(1) full-screen scroll
- SIMD-accelerated VT parser (three-layer: NEON → CSI fast path → state machine)
- xterm-256color subset sufficient for tmux, vim, htop
- Single-threaded: non-blocking PTY I/O with kqueue, no mutexes


## Install

Edit `src/config.rs` and run `make install`

`brew install --cask font-hack`
