pub mod charset;
pub mod csi_fast;
pub mod perform;
pub mod simd;
pub mod state_machine;
pub mod table;
pub mod utf8;

use crate::parser::csi_fast::CsiFastParser;
use crate::parser::perform::Perform;
use crate::parser::simd::SimdScanner;
use crate::parser::state_machine::StateMachine;
use crate::parser::utf8::Utf8Assembler;

/// Main parser: dispatches between SIMD fast path, CSI fast path, and scalar state machine.
pub struct Parser {
    state_machine: StateMachine,
    utf8: Utf8Assembler,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state_machine: StateMachine::new(),
            utf8: Utf8Assembler::new(),
        }
    }

    /// Parse a chunk of bytes, calling methods on the performer.
    pub fn parse<P: Perform>(&mut self, data: &[u8], performer: &mut P) {
        let len = data.len();
        let mut pos = 0;

        // Complete any buffered UTF-8 sequence from the previous parse() call.
        // This must happen before the state machine check — UTF-8 is only buffered
        // when the state machine is in ground state.
        if let Some((ch, consumed)) = self.utf8.try_complete(&data[pos..]) {
            performer.print(ch);
            pos += consumed;
        } else if self.utf8.has_pending() {
            // try_complete consumed all remaining bytes into its buffer but the
            // sequence is still incomplete. Don't reprocess those bytes.
            return;
        }

        // If we're in the middle of a state machine sequence, finish it first
        if !self.state_machine.is_ground() {
            while pos < len {
                let byte = data[pos];
                pos += 1;
                self.state_machine.advance(byte, performer);
                if self.state_machine.is_ground() {
                    break;
                }
            }
        }

        // Main SIMD-accelerated loop
        while pos < len {
            // If state machine is in a non-ground state, keep feeding it
            // until it returns to ground before trying any fast paths.
            if !self.state_machine.is_ground() {
                while pos < len {
                    let byte = data[pos];
                    pos += 1;
                    self.state_machine.advance(byte, performer);
                    if self.state_machine.is_ground() {
                        break;
                    }
                }
                continue;
            }

            // Try SIMD scanner: find the next run of printable ASCII
            let remaining = &data[pos..];

            if remaining.len() >= 16 {
                let (ascii_end, _special_pos) = SimdScanner::scan(remaining);

                if ascii_end > 0 {
                    // Bulk ASCII run
                    performer.print_ascii_run(&remaining[..ascii_end]);
                    pos += ascii_end;
                    if pos >= len {
                        break;
                    }
                }

                // If we're now at a high byte, scan for a mixed text run
                // (ASCII + UTF-8) and process it inline — avoids re-entering
                // the outer loop for every UTF-8 character.
                let remaining = &data[pos..];
                if remaining.len() >= 16 && remaining[0] >= 0x80 {
                    let text_end = SimdScanner::scan_text(remaining);
                    if text_end > 0 {
                        let consumed = Self::process_text_run(&remaining[..text_end], performer);
                        pos += consumed;
                        if pos >= len {
                            break;
                        }
                        if consumed == text_end {
                            continue;
                        }
                        // consumed < text_end: incomplete UTF-8 at end of text run.
                        // Fall through to the scalar UTF-8 handler which will use
                        // the assembler to buffer it for the next parse() call.
                    }
                }
            }

            // Process the next byte
            let byte = data[pos];

            if byte == 0x1B && pos + 1 < len && data[pos + 1] == b'[' {
                // ESC [ detected — enter tight styled text loop.
                // Handles this CSI plus subsequent text + CSI without returning
                // to the outer loop for each transition.
                let consumed = Self::process_styled_run(&data[pos..], performer);
                if consumed > 0 {
                    pos += consumed;
                    continue;
                }
                // First CSI not handleable — feed ESC [ to state machine
                self.state_machine.advance(0x1B, performer);
                self.state_machine.advance(b'[', performer);
                pos += 2;
                continue;
            }

            // Printable ASCII (scalar fallback for short runs / when not aligned)
            if (0x20..0x7F).contains(&byte) {
                // Scan forward for a short ASCII run
                let start = pos;
                while pos < len && data[pos] >= 0x20 && data[pos] < 0x7F {
                    pos += 1;
                }
                performer.print_ascii_run(&data[start..pos]);
                continue;
            }

            // UTF-8 multi-byte sequence
            if (0x80..0xC0).contains(&byte) {
                // Unexpected continuation byte — emit replacement
                performer.print('\u{FFFD}');
                pos += 1;
                continue;
            }
            if byte >= 0xC0 {
                match self.utf8.decode(&data[pos..]) {
                    Some((ch, consumed)) => {
                        performer.print(ch);
                        pos += consumed;
                    }
                    None => {
                        // Incomplete — bytes buffered by assembler for next parse() call
                        break;
                    }
                }
                continue;
            }

            // Control character or ESC — feed to state machine
            self.state_machine.advance(byte, performer);
            pos += 1;
        }
    }

    /// Process styled text: interleaved printable ASCII, UTF-8, CSI sequences,
    /// and line endings (CR/LF) in a tight inner loop. Avoids the overhead of
    /// returning to the main parser loop for each text↔CSI transition.
    ///
    /// Returns the number of bytes consumed. Bails (returns current position)
    /// when it hits something it can't handle inline: bare ESC without `[`,
    /// incomplete CSI, incomplete UTF-8 at end of buffer, or non-CR/LF
    /// control characters.
    fn process_styled_run<P: Perform>(data: &[u8], performer: &mut P) -> usize {
        let len = data.len();
        let mut pos = 0;

        while pos < len {
            let b = data[pos];

            // Printable ASCII run — use SIMD when possible
            if b >= 0x20 && b < 0x7F {
                let start = pos;
                let remaining = &data[pos..];
                if remaining.len() >= 16 {
                    let (ascii_end, _) = SimdScanner::scan(remaining);
                    pos += ascii_end;
                } else {
                    pos += 1;
                    while pos < len && data[pos] >= 0x20 && data[pos] < 0x7F {
                        pos += 1;
                    }
                }
                performer.print_ascii_run(&data[start..pos]);
                continue;
            }

            // ESC [ — CSI sequence
            if b == 0x1B && pos + 2 < len && data[pos + 1] == b'[' {
                let csi_buf = &data[pos + 2..];

                // Try ultra-fast inline SGR first (handles ~90% of SGR sequences)
                if let Some(consumed) = CsiFastParser::try_csi_inline(csi_buf, performer) {
                    pos += 2 + consumed;
                    continue;
                }

                // Try full CSI fast path
                if let Some(consumed) = CsiFastParser::try_parse(csi_buf, performer) {
                    pos += 2 + consumed;
                    continue;
                }

                // CSI not recognized or incomplete — bail to main loop
                break;
            }

            // UTF-8 multi-byte sequence — handle inline so pane borders,
            // emoji, and other non-ASCII text stay in the tight loop.
            if b >= 0xC0 {
                match Self::decode_utf8(&data[pos..]) {
                    Some((ch, consumed)) => {
                        performer.print(ch);
                        pos += consumed;
                        continue;
                    }
                    None => break, // incomplete at buffer end — bail for assembler
                }
            }
            if b >= 0x80 {
                // Stray continuation byte (0x80..0xBF)
                performer.print('\u{FFFD}');
                pos += 1;
                continue;
            }

            // CR/LF — handle inline to stay in the tight loop across line boundaries
            if b == 0x0A || b == 0x0D {
                performer.execute(b);
                pos += 1;
                continue;
            }

            // Anything else (other controls, bare ESC) — bail to main loop
            break;
        }

        pos
    }

    /// Process a mixed text run (ASCII + UTF-8) in a tight loop.
    ///
    /// The input `data` must contain only "text" bytes (>= 0x20, != 0x7F) as
    /// identified by `SimdScanner::scan_text`. Returns the number of bytes consumed.
    /// May return less than `data.len()` if an incomplete UTF-8 sequence is at the end.
    fn process_text_run<P: Perform>(data: &[u8], performer: &mut P) -> usize {
        let len = data.len();
        let mut pos = 0;

        while pos < len {
            if data[pos] < 0x80 {
                // ASCII sub-run — batch via print_ascii_run
                let start = pos;
                pos += 1;
                while pos < len && data[pos] < 0x80 {
                    pos += 1;
                }
                performer.print_ascii_run(&data[start..pos]);
            } else if data[pos] >= 0xC0 {
                // UTF-8 lead byte — inline decode (no assembler)
                match Self::decode_utf8(&data[pos..]) {
                    Some((ch, consumed)) => {
                        performer.print(ch);
                        pos += consumed;
                    }
                    None => break, // incomplete at end of run
                }
            } else {
                // Stray continuation byte (0x80..0xBF)
                performer.print('\u{FFFD}');
                pos += 1;
            }
        }

        pos
    }

    /// Stateless inline UTF-8 decode. Does not buffer incomplete sequences.
    /// Returns None if the sequence is incomplete (fewer bytes than expected).
    #[inline]
    fn decode_utf8(data: &[u8]) -> Option<(char, usize)> {
        let first = data[0];
        let expected = match first {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return Some(('\u{FFFD}', 1)),
        };

        if data.len() < expected {
            return None; // incomplete — caller will handle
        }

        for &byte in data.iter().take(expected).skip(1) {
            if byte & 0xC0 != 0x80 {
                return Some(('\u{FFFD}', 1));
            }
        }

        let mask: u8 = match expected {
            2 => 0x1F,
            3 => 0x0F,
            _ => 0x07,
        };
        let mut cp = (first & mask) as u32;
        for byte in data.iter().take(expected).skip(1) {
            cp = (cp << 6) | (byte & 0x3F) as u32;
        }

        let valid = match expected {
            2 => cp >= 0x80,
            3 => cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp),
            4 => (0x10000..=0x10FFFF).contains(&cp),
            _ => false,
        };

        if valid {
            // SAFETY: validated above — overlong, surrogates, and out-of-range rejected.
            Some((unsafe { char::from_u32_unchecked(cp) }, expected))
        } else {
            Some(('\u{FFFD}', 1))
        }
    }
}
