/// UTF-8 codepoint assembler.
/// Buffers incomplete multi-byte sequences across parse() calls.
pub struct Utf8Assembler {
    buf: [u8; 4],
    len: u8,
}

impl Default for Utf8Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Utf8Assembler {
    pub fn new() -> Self {
        Self {
            buf: [0; 4],
            len: 0,
        }
    }

    /// Returns true if there are buffered bytes awaiting completion.
    pub fn has_pending(&self) -> bool {
        self.len > 0
    }

    /// If there are buffered bytes from a previous parse() call, try to
    /// complete the sequence using new data.
    /// Returns Some((char, new_bytes_consumed)) or None if still incomplete / nothing buffered.
    pub fn try_complete(&mut self, data: &[u8]) -> Option<(char, usize)> {
        if self.len == 0 {
            return None;
        }

        let expected = match self.buf[0] {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => {
                // Invalid start byte somehow buffered — discard
                self.len = 0;
                return Some(('\u{FFFD}', 0));
            }
        };

        let need = expected - self.len as usize;
        let available = data.len().min(need);

        // Validate and copy new continuation bytes
        for (i, &byte) in data.iter().enumerate().take(available) {
            if byte & 0xC0 != 0x80 {
                // Invalid continuation — emit replacement for the buffered bytes,
                // don't consume the invalid byte (caller will handle it)
                self.len = 0;
                return Some(('\u{FFFD}', i));
            }
            self.buf[self.len as usize + i] = byte;
        }
        self.len += available as u8;

        if (self.len as usize) < expected {
            // Still incomplete — need more data (very rare: tiny reads)
            return None;
        }

        // We have all bytes — decode
        let ch = Self::decode_buf(&self.buf, expected);
        self.len = 0;
        Some((ch, available))
    }

    /// Attempt to decode a UTF-8 codepoint from the start of the slice.
    /// Returns Some((char, bytes_consumed)) on success, None if the sequence
    /// is incomplete (bytes are buffered internally for the next call).
    pub fn decode(&mut self, data: &[u8]) -> Option<(char, usize)> {
        if data.is_empty() {
            return None;
        }

        let first = data[0];
        let expected = match first {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return Some(('\u{FFFD}', 1)), // Not a valid start byte
        };

        if data.len() < expected {
            // Incomplete — buffer for next parse() call
            let n = data.len().min(4);
            self.buf[..n].copy_from_slice(&data[..n]);
            self.len = n as u8;
            return None;
        }

        // Validate continuation bytes
        for &byte in data.iter().take(expected).skip(1) {
            if byte & 0xC0 != 0x80 {
                return Some(('\u{FFFD}', 1)); // Invalid continuation
            }
        }

        let ch = Self::decode_buf(data, expected);
        Some((ch, expected))
    }

    fn decode_buf(buf: &[u8], expected: usize) -> char {
        let first = buf[0];
        let mask: u8 = match expected {
            2 => 0x1F,
            3 => 0x0F,
            4 => 0x07,
            _ => return '\u{FFFD}',
        };

        let mut cp = (first & mask) as u32;
        for byte in buf.iter().take(expected).skip(1) {
            cp = (cp << 6) | (byte & 0x3F) as u32;
        }

        // Reject overlong encodings and surrogates
        let valid = match expected {
            2 => cp >= 0x80,
            3 => cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp),
            4 => (0x10000..=0x10FFFF).contains(&cp),
            _ => false,
        };

        if valid {
            // SAFETY: `cp` has been validated above — overlong encodings, surrogates
            // (0xD800..=0xDFFF), and values > 0x10FFFF are all rejected, so cp is a
            // valid Unicode scalar value.
            unsafe { char::from_u32_unchecked(cp) }
        } else {
            '\u{FFFD}'
        }
    }
}
