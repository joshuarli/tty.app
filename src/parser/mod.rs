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
            }

            // Process the next byte
            let byte = data[pos];

            if byte == 0x1B && pos + 1 < len && data[pos + 1] == b'[' {
                // ESC [ detected — try CSI fast path
                pos += 2; // skip ESC [
                let remaining = &data[pos..];
                match CsiFastParser::try_parse(remaining, performer) {
                    Some(consumed) => {
                        pos += consumed;
                        continue;
                    }
                    None => {
                        // CSI fast path failed, feed ESC and [ to state machine
                        self.state_machine.advance(0x1B, performer);
                        self.state_machine.advance(b'[', performer);
                        continue;
                    }
                }
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
                // Start of multi-byte UTF-8
                if let Some((ch, consumed)) = self.utf8.decode(&data[pos..]) {
                    performer.print(ch);
                    pos += consumed;
                } else {
                    // Incomplete — need more data. For now, emit replacement.
                    performer.print('\u{FFFD}');
                    pos += 1;
                }
                continue;
            }

            // Control character or ESC — feed to state machine
            self.state_machine.advance(byte, performer);
            pos += 1;
        }
    }
}
