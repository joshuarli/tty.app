use tty::parser::charset::translate_dec_special;

#[test]
fn dec_special_graphics_mapping() {
    assert_eq!(translate_dec_special(0x60), '\u{25C6}');
    assert_eq!(translate_dec_special(0x61), '\u{2592}');
    assert_eq!(translate_dec_special(0x62), '\u{2409}');
    assert_eq!(translate_dec_special(0x63), '\u{240C}');
    assert_eq!(translate_dec_special(0x64), '\u{240D}');
    assert_eq!(translate_dec_special(0x65), '\u{240A}');
    assert_eq!(translate_dec_special(0x66), '\u{00B0}');
    assert_eq!(translate_dec_special(0x67), '\u{00B1}');
    assert_eq!(translate_dec_special(0x68), '\u{2424}');
    assert_eq!(translate_dec_special(0x69), '\u{240B}');
    assert_eq!(translate_dec_special(0x6A), '\u{2518}');
    assert_eq!(translate_dec_special(0x6B), '\u{2510}');
    assert_eq!(translate_dec_special(0x6C), '\u{250C}');
    assert_eq!(translate_dec_special(0x6D), '\u{2514}');
    assert_eq!(translate_dec_special(0x6E), '\u{253C}');
    assert_eq!(translate_dec_special(0x6F), '\u{23BA}');
    assert_eq!(translate_dec_special(0x70), '\u{23BB}');
    assert_eq!(translate_dec_special(0x71), '\u{2500}');
    assert_eq!(translate_dec_special(0x72), '\u{23BC}');
    assert_eq!(translate_dec_special(0x73), '\u{23BD}');
    assert_eq!(translate_dec_special(0x74), '\u{251C}');
    assert_eq!(translate_dec_special(0x75), '\u{2524}');
    assert_eq!(translate_dec_special(0x76), '\u{2534}');
    assert_eq!(translate_dec_special(0x77), '\u{252C}');
    assert_eq!(translate_dec_special(0x78), '\u{2502}');
    assert_eq!(translate_dec_special(0x79), '\u{2264}');
    assert_eq!(translate_dec_special(0x7A), '\u{2265}');
    assert_eq!(translate_dec_special(0x7B), '\u{03C0}');
    assert_eq!(translate_dec_special(0x7C), '\u{2260}');
    assert_eq!(translate_dec_special(0x7D), '\u{00A3}');
    assert_eq!(translate_dec_special(0x7E), '\u{00B7}');
}

#[test]
fn dec_special_non_mapped_passes_through() {
    for b in 0x00..=0x5Fu8 {
        assert_eq!(translate_dec_special(b), b as char, "byte 0x{:02X}", b);
    }
    for b in 0x7Fu8..=0xFF {
        assert_eq!(translate_dec_special(b), b as char, "byte 0x{:02X}", b);
    }
}

#[test]
fn dec_special_all_sixty_thru_seven_e_mapped() {
    let unmapped: Vec<u8> = (0x60u8..=0x7E)
        .filter(|&b| translate_dec_special(b) == b as char)
        .collect();
    assert!(
        unmapped.is_empty(),
        "unmapped DEC special bytes: {:02X?}",
        unmapped
    );
}
