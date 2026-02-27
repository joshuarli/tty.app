/// UTF-8 codepoint assembler.
/// Decodes multi-byte sequences from a byte slice.
pub struct Utf8Assembler {
    // For handling incomplete sequences across buffer boundaries
    buf: [u8; 4],
    buf_len: usize,
    expected_len: usize,
}

impl Utf8Assembler {
    pub fn new() -> Self {
        Self {
            buf: [0; 4],
            buf_len: 0,
            expected_len: 0,
        }
    }

    /// Attempt to decode a UTF-8 codepoint from the start of the slice.
    /// Returns Some((char, bytes_consumed)) or None if the sequence is incomplete.
    pub fn decode(&mut self, data: &[u8]) -> Option<(char, usize)> {
        if data.is_empty() {
            return None;
        }

        let first = data[0];
        let (expected, mask) = match first {
            0xC0..=0xDF => (2, 0x1F),
            0xE0..=0xEF => (3, 0x0F),
            0xF0..=0xF7 => (4, 0x07),
            _ => return None, // Not a valid start byte
        };

        if data.len() < expected {
            // Incomplete sequence — return replacement for now
            // (proper cross-buffer handling would buffer the bytes)
            return Some(('\u{FFFD}', data.len()));
        }

        // Validate continuation bytes
        for i in 1..expected {
            if data[i] & 0xC0 != 0x80 {
                return Some(('\u{FFFD}', 1)); // Invalid continuation
            }
        }

        let mut cp: u32 = (first & mask) as u32;
        for i in 1..expected {
            cp = (cp << 6) | (data[i] & 0x3F) as u32;
        }

        // Reject overlong encodings and surrogates
        let valid = match expected {
            2 => cp >= 0x80,
            3 => cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp),
            4 => cp >= 0x10000 && cp <= 0x10FFFF,
            _ => false,
        };

        if valid {
            Some((unsafe { char::from_u32_unchecked(cp) }, expected))
        } else {
            Some(('\u{FFFD}', expected))
        }
    }
}
