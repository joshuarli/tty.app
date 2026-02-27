# tty.app

my terminal emulator weighs less than 200 kb

- only supports apple silicon
- xterm-256color subset sufficient for tmux
- damage-tracking
- renders every frame in a single Metal compute shader dispatch — no vertex buffers, no draw calls, just one kernel per pixel


## install

edit `src/config.rs` and run `make install`

`brew install --cask font-hack`

