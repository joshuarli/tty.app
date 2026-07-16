# Render instability investigation

Date: 2026-07-16

## Reported reproduction

Open two tmux panes and run `btop -u 150` in both. After one or two seconds,
tty may seize up, become visually stuttery or glitchy, and stop responding to
normal macOS window switching.

The issue has so far only been observed on the current M1 Max machine. The
same macOS version does not show the problem on an M1 Air or M1 Pro.

## Headless reproduction

The existing headless Metal benchmark was used with output captured from two
real `/opt/homebrew/bin/btop -u 150` processes. The canonical 148-column
fixture is too narrow for a horizontal two-pane btop layout, so the capture
used a 320-column tmux session, giving each pane about 160 columns. Both pane
outputs were piped to one raw ANSI capture for replay.

The five-second capture contained approximately 1.7 MiB and replayed as 52
headless frames through the parser, performer, grid, and Metal renderer.

Results on Apple M1 Max:

- Full headless replay: 123.7 ms wall time, 20.6 ms GPU time.
- Cell-tiled renderer: 62.9 ms wall time, 46.1 ms GPU time.
- GPU timings were valid for all frames.
- No Metal command-buffer errors, hangs, or crashes occurred.

The original `target/tmux-less-44x148-both.typescript` fixture was restored
after the run. No source files were changed by the reproduction.

## Interpretation

The workload itself reaches the headless renderer and is substantially heavier
than the existing one-frame fixture, but the GUI freeze does not reproduce in
the headless harness. That harness waits synchronously for each command buffer
and does not exercise the AppKit event loop, CAMetalLayer drawable availability,
or the production renderer's asynchronous double-buffer lifecycle.

The M1 Max-only behavior increases suspicion of a timing- or GPU-scheduling-
dependent Metal issue rather than a deterministic parser failure. The other
Apple Silicon machines running the same macOS version provide a useful control
case, but this is not yet enough to distinguish GPU scheduling from thermal or
load differences.

The most likely production-only failure mode is main-thread starvation while
PTY output remains continuous and a Metal cell buffer is still in flight. The
renderer returns without dispatching in that state, but `needs_render` remains
true, so the main loop does not enter its idle run-loop wait. It can repeatedly
parse PTY data, drain events, and retry rendering without yielding to the
run loop.

There is also a separate asynchronous resource hazard: all command buffers use
one shared uniform buffer even though cell data is double-buffered. A later
frame can overwrite uniforms while an earlier GPU command still reads them.
This is a plausible source of visual corruption, but it does not by itself
explain the loss of AppKit responsiveness as strongly as the busy retry loop.

## Backpressure experiment

Renderer backpressure was tested by briefly disabling PTY wake callbacks and
running the Core Foundation loop whenever the current GPU buffer or drawable
was busy. This added no GPU allocations or framebuffer, but did not resolve the
manual two-pane `btop -u 150` reproduction and was reverted.

The next candidate was moving PTY reads, parsing, and grid updates off the
AppKit thread, following Alacritty's dedicated PTY event-loop thread. Glyph
atlas mutation remains on the main thread because the Metal atlas is UI-owned.
The UI thread should receive a coalesced wakeup and render the latest terminal
state through its normal event loop.

## PTY worker experiment

The PTY worker implementation is now in place for manual verification. Each
terminal has a worker-owned parser, grid, scrollback, and response buffer. The
worker does not touch AppKit, Metal, CoreText, or the main-thread glyph atlas.
After parsing a bounded batch, it signals the main thread through a nonblocking
pipe. The main thread briefly locks the worker state, copies a render snapshot
of the grid and scrollback, resolves newly seen non-ASCII glyphs in the main
atlas, handles terminal responses, and returns to normal rendering.

The worker remains the sole owner of parser state; the snapshot avoids parsing
the next incremental update on stale state. This adds a bounded memory copy per
worker notification and one additional grid and scrollback allocation per live
terminal. Existing library, integration, and headless Metal replay tests pass.
The remaining verification is the original two-pane `btop -u 150` test on the
M1 Max, followed by checking resize, selection, clipboard responses, and shell
exit behavior.

The first manual run exposed severe line corruption. The headless worker
handoff regression reproduced it: replaying the captured btop stream
diverges at chunk 1. The deterministic fallback fixture also fails at chunk 1,
where an incremental update expects content from the previous frame. The
original swap was not a valid concurrent double buffer: after the worker
published its current grid, it received the main thread's older grid and
continued parsing from stale terminal state. The implementation now uses a
render snapshot instead, and the same headless regression passes.

If that improves responsiveness but visual glitches remain, double-buffer the
uniform buffer alongside the cell buffers and bind the matching per-frame
uniform resource.

## Alacritty comparison

The local Alacritty checkout provides a useful control: it is stable under the
same btop workload, but it uses OpenGL rather than tty's Metal renderer, so the
comparison does not directly validate GPU resource handling.

The most relevant robustness differences are architectural:

- Alacritty's PTY reader and parser run on a dedicated thread. The main winit
  thread receives a coalesced `TerminalEvent::Wakeup` instead of parsing PTY
  bytes itself.
- Winit owns the macOS event loop. Alacritty does not manually drain AppKit
  events or substitute a custom outer loop.
- Redraw requests are edge-triggered and coalesced. A window tracks whether a
  redraw is already requested and whether a display frame is available.
- After presenting, Alacritty schedules the next frame using the monitor's
  refresh interval and only requests it again when the terminal is dirty.

tty now moves PTY reads, parsing, grid updates, and response generation off the
AppKit thread, but still drains AppKit events and submits Metal work from its
custom main loop. A deferred Metal render can still leave `needs_render` set and
cause immediate retry spinning, so a remaining follow-up is to coalesce or
rate-limit redraw attempts when a buffer or drawable is unavailable.

## Alacritty frame-gate mechanism

Alacritty uses three mechanisms to avoid main-thread starvation under
continuous PTY output:

### 1. Frame gate (`has_frame`)

`display/window.rs` tracks `has_frame: bool`. It starts true, is set to false
by `request_frame()` after each draw, and is set back to true when a scheduled
`Frame` timer fires. The critical effect: a PTY `Wakeup` event only calls
`request_redraw()` when `has_frame` is available:

```rust
(EventType::Terminal(TerminalEvent::Wakeup), Some(window_id)) => {
    window_context.dirty = true;
    if window_context.display.window.has_frame {
        window_context.display.window.request_redraw();
    }
},
```

If `has_frame` is false, the terminal stays dirty but no redraw is requested.
The dirt accumulates and rendering resumes when the frame timer fires:

```rust
(EventType::Frame, Some(window_id)) => {
    window_context.display.window.has_frame = true;
    if window_context.dirty {
        window_context.display.window.request_redraw();
    }
},
```

### 2. Monitor-synced frame scheduling

`display/mod.rs` `request_frame()` calls `FrameTimer::compute_timeout()` with
the monitor's refresh interval (e.g. 60 Hz → ~16.7 ms). If the next vblank is
in the future, it schedules a deferred `Frame` event instead of drawing
immediately. If we're already late (e.g. after a burst of PTY data), it
schedules immediately with `Duration::ZERO`:

```rust
let next_frame = self.last_synced_timestamp + self.refresh_interval;
if next_frame < now {
    self.last_synced_timestamp = now - D::from_micros(elapsed_micros % refresh_micros);
    Duration::ZERO
} else {
    self.last_synced_timestamp = next_frame;
    next_frame - now
}
```

This ensures redraws never exceed the display refresh rate. The scheduler feeds
`ControlFlow::WaitUntil(deadline)` into winit's event loop so the thread blocks
between frames instead of spinning.

### 3. Non-blocking swap

`SwapInterval::DontWait` disables vsync on swap. The buffer exchange returns
immediately even if the compositor hasn't consumed the previous frame. Combined
with the frame gate, this prevents the render path from ever blocking the main
thread.

### Effect in tty terms

In tty's loop, `needs_render` being true causes an immediate retry with no
delay. Alacritty's equivalent state is `dirty = true` with `has_frame = false`.
Instead of spinning, the event loop blocks until the scheduled `Frame` timer
fires at the next display refresh boundary. The GPU has an entire refresh
interval to complete, and PTY data accumulated during that interval is
coalesced into a single render pass.
