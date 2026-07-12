use tty::parser::utf8::Utf8Assembler;

#[test]
fn single_byte_ascii_not_handled_by_assembler() {
    let mut asm = Utf8Assembler::new();
    assert!(!asm.has_pending());
    assert!(asm.try_complete(b"a").is_none());
}

#[test]
fn complete_two_byte_sequence() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xC3\xA9");
    assert_eq!(result, Some(('\u{00E9}', 2)));
    assert!(!asm.has_pending());
}

#[test]
fn complete_three_byte_sequence() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xE2\x82\xAC");
    assert_eq!(result, Some(('\u{20AC}', 3)));
    assert!(!asm.has_pending());
}

#[test]
fn complete_four_byte_sequence() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xF0\x9F\x92\xA9");
    assert_eq!(result, Some(('\u{1F4A9}', 4)));
    assert!(!asm.has_pending());
}

#[test]
fn invalid_start_byte_emits_replacement() {
    let mut asm = Utf8Assembler::new();
    assert_eq!(asm.decode(b"\x80"), Some(('\u{FFFD}', 1)));
    assert_eq!(asm.decode(b"\xBF"), Some(('\u{FFFD}', 1)));
}

#[test]
fn valid_start_byte_incomplete_buffers() {
    let mut asm = Utf8Assembler::new();
    // \xC0 is a valid 2-byte start byte; incomplete 1-byte input buffers
    assert!(asm.decode(b"\xC0").is_none());
    assert!(asm.has_pending());
}

#[test]
fn invalid_continuation_byte_emits_replacement() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xC3\x20");
    assert_eq!(result, Some(('\u{FFFD}', 1)));
}

#[test]
fn two_byte_split_across_boundary() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xC3").is_none());
    assert!(asm.has_pending());
    let result = asm.try_complete(b"\xA9");
    assert_eq!(result, Some(('\u{00E9}', 1)));
    assert!(!asm.has_pending());
}

#[test]
fn three_byte_split_across_two_calls() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xE2").is_none());
    assert!(asm.has_pending());
    assert!(asm.try_complete(b"\x82").is_none());
    assert!(asm.has_pending());
    let result = asm.try_complete(b"\xAC");
    assert_eq!(result, Some(('\u{20AC}', 1)));
    assert!(!asm.has_pending());
}

#[test]
fn four_byte_split_one_at_a_time() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xF0").is_none());
    assert!(asm.try_complete(b"\x9F").is_none());
    assert!(asm.try_complete(b"\x92").is_none());
    let result = asm.try_complete(b"\xA9");
    assert_eq!(result, Some(('\u{1F4A9}', 1)));
}

#[test]
fn invalid_continuation_in_buffered_sequence() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xE2\x82").is_none());
    let result = asm.try_complete(b"\x20");
    assert_eq!(result, Some(('\u{FFFD}', 0)));
    assert!(!asm.has_pending());
}

#[test]
fn buffered_overlong_two_byte_detected_on_completion() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xC0").is_none());
    assert!(asm.has_pending());
    // \xC0\x80 is a 2-byte overlong encoding of NUL; full sequence is consumed
    let result = asm.try_complete(b"\x80");
    assert_eq!(result, Some(('\u{FFFD}', 1)));
    assert!(!asm.has_pending());
}

#[test]
fn overlong_two_byte_encoding_rejected() {
    let mut asm = Utf8Assembler::new();
    // \xC0 is a valid 2-byte start byte; all `expected` bytes are consumed
    let result = asm.decode(b"\xC0\x80");
    assert_eq!(result, Some(('\u{FFFD}', 2)));
}

#[test]
fn overlong_three_byte_encoding_rejected() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xE0\x80\x80");
    assert_eq!(result, Some(('\u{FFFD}', 3)));
}

#[test]
fn overlong_four_byte_encoding_rejected() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xF0\x80\x80\x80");
    assert_eq!(result, Some(('\u{FFFD}', 4)));
}

#[test]
fn surrogate_encoding_rejected() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xED\xA0\x80");
    assert_eq!(result, Some(('\u{FFFD}', 3)));
}

#[test]
fn out_of_range_encoding_rejected() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xF4\x90\x80\x80");
    assert_eq!(result, Some(('\u{FFFD}', 4)));
}

#[test]
fn decode_buf_invalid_expected() {
    let mut asm = Utf8Assembler::new();
    let result = asm.decode(b"\xFE");
    assert_eq!(result, Some(('\u{FFFD}', 1)));
}

#[test]
fn empty_data_returns_none() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"").is_none());
}

#[test]
fn fresh_assembler_has_no_pending() {
    let asm = Utf8Assembler::new();
    assert!(!asm.has_pending());
}

#[test]
fn try_complete_on_empty_assembler_returns_none() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.try_complete(b"hello").is_none());
}

#[test]
fn multiple_complete_sequences_in_a_row() {
    let mut asm = Utf8Assembler::new();
    assert_eq!(asm.decode(b"\xC3\xA9"), Some(('\u{00E9}', 2)));
    assert_eq!(asm.decode(b"\xC3\xA9"), Some(('\u{00E9}', 2)));
    assert!(!asm.has_pending());
}

#[test]
fn three_byte_sequence_split_after_second_byte() {
    let mut asm = Utf8Assembler::new();
    assert!(asm.decode(b"\xE2\x82").is_none());
    let result = asm.try_complete(b"\xAC");
    assert_eq!(result, Some(('\u{20AC}', 1)));
    assert!(!asm.has_pending());
}

#[test]
fn leftover_stale_bytes_after_error() {
    let mut asm = Utf8Assembler::new();
    assert_eq!(asm.decode(b"\x80"), Some(('\u{FFFD}', 1)));
    assert_eq!(asm.decode(b"\xC3\xA9"), Some(('\u{00E9}', 2)));
}
