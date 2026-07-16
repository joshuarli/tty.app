# Color architecture

tty keeps color data inside the fixed 8-byte cell shared directly by Rust and
Metal. This is intentional: dirty rows can be copied to the GPU with a raw
memcpy, without a render-time cell conversion or packing pass.

## Cell contract

The color fields are two 8-bit palette indices:

```text
offset  size  field
4       1     foreground palette index
5       1     background palette index
```

`Cell` has no room for per-cell RGB values or an alpha value. The remaining
bytes are the codepoint, flags, and glyph-atlas coordinates. Any future color
feature must therefore use palette indirection or existing flag bits rather
than expanding the cell.

The renderer currently provides a 256-entry xterm-style palette. Palette
entries are uploaded as `half4` values, but their alpha is always `1.0`.
Glyph-atlas alpha is coverage: it blends glyph foreground with the cell
background. It is not per-cell color transparency.

## Current behavior

### Indexed colors

Indexed colors are the native representation:

- ANSI colors `30..37`, `40..47`, `90..97`, and `100..107` select palette
  entries directly.
- `38;5;n` and `48;5;n` select one of the 256 entries.
- The palette contains 16 ANSI colors, 216 color-cube entries, and 24
  grayscale entries.

The cell stores the resulting index, so indexed-color handling is a single
byte assignment on the parser/performer path and a palette lookup in Metal.

### Truecolor

The parser recognizes `38;2;r;g;b` and `48;2;r;g;b`, including the fast CSI
path. tty does not preserve the RGB value. `rgb_to_palette` computes the
nearest entry in the configured 256-color palette using squared RGB distance,
then stores that index in the cell.

The current conversion is allocation-free, but it scans all 256 entries. For
each entry it performs three channel differences, three integer multiplies,
and additions. This work occurs when a color escape is parsed, not for every
printed character or every rendered cell. `color_256` itself is only a `u8`
assignment.

### Dimming

SGR `2` sets the `DIM` cell flag and SGR `22` clears both `BOLD` and `DIM`.
The Metal shader currently multiplies foreground RGB by `0.66` when the flag
is set. Background RGB is not dimmed. This preserves the original palette
index but adds a per-pixel multiply in the shader.

### Alpha and transparency

There is no per-cell alpha or terminal truecolor alpha representation. The
shader writes opaque palette colors. Alpha-like behavior currently consists
of glyph coverage, hidden text (`fg = bg`), inverse/selection swaps, and the
dim multiplier. Window or frame transparency is separate from cell color and
is not represented in `Cell`.

## Performance and precomputation

The CPU RGB conversion is not a render hot path in the usual sense: terminal
programs emit color-changing SGR sequences much less often than printable
characters, and the conversion performs no allocation or heap churn.

A complete RGB-to-index lookup table would be:

| Input precision | Entries | Storage at 1 byte/entry |
| --- | ---: | ---: |
| 4 bits/channel | 4,096 | 4 KiB |
| 5 bits/channel | 32,768 | 32 KiB |
| 6 bits/channel | 262,144 | 256 KiB |
| 8 bits/channel | 16,777,216 | 16 MiB |

A reduced table can quantize each input channel before lookup, for example
`(r >> 2, g >> 2, b >> 2)` for 6 bits/channel. This makes conversion O(1) with
small bounded input quantization, but it may choose a different nearest
palette entry than the current exact 8-bit calculation.

The full 16 MiB table should not be generated naïvely at startup. Building it
by comparing every RGB input against all 256 palette entries requires about
4.3 billion distance comparisons. If exact mapping is ever required, the
table should be generated offline or built with a specialized nearest-color
algorithm. Until conversion is shown to matter, the current allocation-free
scan is the simpler choice.

Precomputing dim colors is inexpensive in memory, but it is not automatically
faster. The focused Metal benchmark showed that the extra palette selection
was slower than the original arithmetic path. The palette is small and likely
cache-resident, but a dynamic/conditional lookup still adds address and
dependency work. The `half` RGB multiply is preferable here.

## 8-bit-compatible direction

The preferred design is now partly implemented:

1. Keep normal and truecolor-derived cell colors as 8-bit palette indices.
2. Keep dim state in `CellFlags`; do not spend palette indices on dim colors.
3. Keep dimming as a `half` RGB multiply in the shader. Implemented.
4. Use the focused `metal_color` benchmark to validate future changes.
5. Consider a 6-bit/channel RGB-to-index LUT only if parser throughput or
   profiling demonstrates that the 256-entry scan is material.

This supports indexed colors, palette-quantized truecolor, and dimming without
changing the cell/GPU ABI. It does not provide arbitrary per-cell RGB or
alpha. Supporting those would require an auxiliary color buffer, a larger
cell, or a more elaborate palette allocation scheme.

## Comparison with Alacritty

The local Alacritty checkout uses a different tradeoff. Its terminal cell can
retain `Named`, `Indexed`, or full `Rgb` colors, so it does not need to
quantize truecolor into an 8-bit cell. Its display layer builds a color list
up front, including configured or automatically derived dim colors. Named and
indexed colors become table lookups; dimmed explicit RGB colors still apply a
`0.66` RGB multiplication during render preparation.

Alacritty also carries background alpha separately for window/background
transparency. That is possible because its renderable-cell representation is
not constrained to tty's fixed 8-byte GPU format.

The relevant tty implementation is in `src/terminal/cell.rs`,
`src/config.rs`, `src/perform_shared.rs`, and `src/renderer/shader.metal`.

## Color fixtures

The shell fixtures used for manual terminal testing are vendored in
`scripts/colors/24-bit-color.sh` and `scripts/colors/color-test.sh`. The first
is the John Morales/iTerm2-style 24-bit gradient; the second covers ANSI,
indexed 256-color, grayscale, and truecolor modes.

The headless Metal test
`truecolor_gradient_writes_ppm_and_matches_quantized_palette` feeds the
gradient escape sequences through the real parser and performer, renders the
result, writes `target/truecolor-fixture.ppm`, and checks representative cell
pixels against the palette entries selected by `rgb_to_palette`. This is a
parser-to-renderer regression fixture, but it is intentionally not yet a
truecolor-fidelity test: the current architecture quantizes truecolor before
the cell is written. The PPM is a compact 4x4-pixel-per-cell visualization;
the test still renders and checks the full-resolution Metal output before
downsampling it. The compact golden PPM is checked in as
`tests/fixtures/truecolor-fixture.ppm.gz`; the test shells out to `gunzip` and
compares the generated bytes with that fixture.

## Follow-up: Alacritty oracle

- Build or use a headless Alacritty capture harness from the local checkout at
  `/Users/josh/d/alacritty`.
- Feed both terminals the identical ANSI fixture bytes.
- Match cell dimensions, font, scale factor, palette/configuration, and output
  dimensions.
- Export Alacritty's rendered pixels as a PPM and compare it with
  `target/truecolor-fixture.ppm`.
- Use the comparison to separate expected palette quantization from renderer
  differences and to measure the visual benefit of future truecolor support.
