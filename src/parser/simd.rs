/// NEON SIMD byte classifier for fast ASCII scanning.
///
/// Classifies 16 bytes at a time: printable ASCII (0x20..0x7E) are "fast",
/// everything else (control, DEL, high bytes) needs attention.
pub struct SimdScanner;

impl SimdScanner {
    /// Scan the buffer for a contiguous run of printable ASCII.
    /// Returns (ascii_run_length, first_special_position).
    ///
    /// ascii_run_length: number of bytes from the start that are printable ASCII.
    /// first_special_position: position of the first non-printable byte (or buf.len()).
    #[cfg(target_arch = "aarch64")]
    pub fn scan(buf: &[u8]) -> (usize, usize) {
        use core::arch::aarch64::*;

        let len = buf.len();
        let mut pos = 0;

        // Process 64 bytes at a time (4 × 16 unrolled)
        unsafe {
            let lo = vdupq_n_u8(0x20);
            let hi = vdupq_n_u8(0x7F);

            while pos + 64 <= len {
                let ptr = buf.as_ptr().add(pos);

                let c0 = vld1q_u8(ptr);
                let c1 = vld1q_u8(ptr.add(16));
                let c2 = vld1q_u8(ptr.add(32));
                let c3 = vld1q_u8(ptr.add(48));

                // Check: byte >= 0x20 && byte < 0x7F  (i.e., printable ASCII)
                let ok0 = vandq_u8(vcgeq_u8(c0, lo), vcltq_u8(c0, hi));
                let ok1 = vandq_u8(vcgeq_u8(c1, lo), vcltq_u8(c1, hi));
                let ok2 = vandq_u8(vcgeq_u8(c2, lo), vcltq_u8(c2, hi));
                let ok3 = vandq_u8(vcgeq_u8(c3, lo), vcltq_u8(c3, hi));

                // AND all together — if all 64 bytes are printable, all bits are 0xFF
                let all = vandq_u8(vandq_u8(ok0, ok1), vandq_u8(ok2, ok3));

                // Check if all 64 bytes are printable
                if vminvq_u8(all) == 0xFF {
                    pos += 64;
                    continue;
                }

                // Some byte needs attention — find which chunk and which byte
                if vminvq_u8(ok0) != 0xFF {
                    return (pos + Self::find_first_zero(ok0), pos + Self::find_first_zero(ok0));
                }
                pos += 16;
                if vminvq_u8(ok1) != 0xFF {
                    return (pos + Self::find_first_zero(ok1), pos + Self::find_first_zero(ok1));
                }
                pos += 16;
                if vminvq_u8(ok2) != 0xFF {
                    return (pos + Self::find_first_zero(ok2), pos + Self::find_first_zero(ok2));
                }
                pos += 16;
                return (pos + Self::find_first_zero(ok3), pos + Self::find_first_zero(ok3));
            }

            // Process remaining 16-byte chunks
            while pos + 16 <= len {
                let c = vld1q_u8(buf.as_ptr().add(pos));
                let ok = vandq_u8(vcgeq_u8(c, lo), vcltq_u8(c, hi));
                if vminvq_u8(ok) == 0xFF {
                    pos += 16;
                } else {
                    let off = Self::find_first_zero(ok);
                    return (pos + off, pos + off);
                }
            }
        }

        // Scalar tail
        while pos < len {
            let b = buf[pos];
            if !(0x20..0x7F).contains(&b) {
                return (pos, pos);
            }
            pos += 1;
        }

        (pos, pos)
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn find_first_zero(v: core::arch::aarch64::uint8x16_t) -> usize {
        use core::arch::aarch64::*;
        // Find first byte that is 0x00 (not 0xFF)
        // Narrow to 8 bytes, then extract and find trailing zeros
        let narrowed = unsafe { vshrn_n_u16::<4>(vreinterpretq_u16_u8(v)) };
        let bits = unsafe { vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed)) };
        // Each byte in narrowed is 0x0F (ok) or 0x00 (attention)
        // Find first 0x00 nibble
        if bits == 0xFFFF_FFFF_FFFF_FFFF {
            return 16; // all ok (shouldn't happen if called correctly)
        }
        // Each nibble represents one byte. Find first zero nibble.
        for i in 0..16 {
            if ((bits >> (i * 4)) & 0xF) == 0 {
                return i;
            }
        }
        16
    }

    /// Scalar fallback for non-aarch64 (shouldn't be used on Apple Silicon).
    #[cfg(not(target_arch = "aarch64"))]
    pub fn scan(buf: &[u8]) -> (usize, usize) {
        let mut pos = 0;
        while pos < buf.len() {
            let b = buf[pos];
            if b < 0x20 || b >= 0x7F {
                return (pos, pos);
            }
            pos += 1;
        }
        (pos, pos)
    }
}
