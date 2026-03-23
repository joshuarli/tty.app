# VT Parser — Design Notes

## What was tried and didn't work

### simdjson-style "classify once, iterate bitmask" for process_styled_run (2026-03)

Replaced the scan-process-scan cycle in `process_styled_run` with a single-pass approach: classify 64 bytes via NEON into a u64 bitmask of non-printable positions, then iterate set bits with `trailing_zeros()` to dispatch ESC/control/UTF-8 without re-scanning between CSI sequences.

Bitmask extraction used the vaddv (AND with powers-of-2, horizontal sum per half) technique to emulate x86 `movmskb` on NEON. Added a `vminvq` early-return for all-printable windows to avoid extraction when mask=0.

**Results** (vs baseline, 13 parser benchmarks):

- 256-color heavy: -2% to -4% (the only clear win — ~4 ESCs per 64-byte window)
- git diff color, truecolor: -0.5% to -0.7% (marginal)
- fullscreen redraw, mixed CSI: **+11-14%** (significant regression)
- tmux pane redraw: **+5%**
- claude code TUI, ls output: **+3-4%**

**Why it lost**: Two compounding issues on NEON:

1. **Expensive extraction**: no `movmskb`. The cheapest approach (vaddv) costs ~32 SIMD ops per 64-byte window on top of classification. This exceeds the savings from avoiding repeated `scan()` calls unless there are 4+ structural characters per window. Most terminal workloads have 1-3.

2. **Fixed window size**: the bitmask forces 64-byte windows, requiring re-classification at each boundary. The existing `scan()` naturally adapts — scanning hundreds of printable bytes in one call for long ASCII runs (the common case in fullscreen redraws). The bitmask approach turns one `scan()` call into 2-3 classify+extract calls.

**Conclusion**: on NEON, the adaptive scan (range check + `vminvq` all-ones fast path + `find_first_zero`) is better for VT parsing. The bitmask approach would likely work on x86 where `movmskb` makes extraction free. If revisiting, try the narrowing approach (`vshrn` → nibble-per-byte, ~2 ops per 16 bytes) instead of vaddv, but the fixed-window problem remains.
