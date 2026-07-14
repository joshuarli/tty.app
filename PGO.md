# Profile-guided optimization

The Makefile owns the automated PGO workflow. This document covers the part
that cannot be supplied by benchmarks: recordings of real terminal sessions.

## Manual session capture

Capture a session with macOS `script`, then use the terminal normally:

```sh
mkdir -p sessions
script -q sessions/interactive.raw

# Inside the session, exercise the workloads you want represented:
# cat large files, run colored commands, use vim/tmux, scroll, resize, etc.
# Exit the shell when finished.
```

The recording should contain terminal output, not a screenshot or application
log. Keep recordings representative and reasonably bounded; they are replayed
for every profiling run.

## Suggested recordings

| Recording | Workload | Covers |
|---|---|---|
| `sessions/bulk-ascii.raw` | `cat` a large source file | Bulk shell output |
| `sessions/heavy-sgr.raw` | Recursive colored directory listing | SGR and palette changes |
| `sessions/interactive.raw` | vim or tmux session | Cursor movement, alternate screen, scroll regions |

## Replay harness

Add recordings to a replay benchmark or test rather than profiling them only
through a live window. The harness should feed the bytes through the real
parser, performer, grid, scrollback, and renderer paths in the same chunk sizes
used by PTY reads, for example:

```rust
let data = std::fs::read("sessions/bulk-ascii.raw").unwrap();
for chunk in data.chunks(64 * 1024) {
    parser.parse(chunk, &mut performer);
}
```

For renderer work, preserve the resulting dirty rows and frame boundaries so
the replay can compare full rendering with damage-driven rendering. A useful
replay harness should also verify that optimized paths produce the same final
grid and pixels as the reference path.
