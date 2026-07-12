pub(crate) fn is_wide(cp: u32) -> bool {
    matches!(cp,
        // East Asian Wide
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33BF |
        0x3400..=0x4DBF | 0x4E00..=0xA4CF | 0xA960..=0xA97C |
        0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE10..=0xFE6F |
        0xFF01..=0xFF60 | 0xFFE0..=0xFFE6 |
        // Supplementary CJK
        0x20000..=0x2FA1F |
        // Emoji (wide per UAX #11)
        0x1F000..=0x1F02F | 0x1F0A0..=0x1F0FF |
        0x1F300..=0x1F9FF | 0x1FA00..=0x1FA6F | 0x1FA70..=0x1FAFF
    )
}

pub(crate) fn is_zero_width(cp: u32) -> bool {
    matches!(cp,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD |
        0x0610..=0x061A | 0x064B..=0x065F | 0x0670 |
        0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 |
        0x06EA..=0x06ED | 0x0711 | 0x0730..=0x074A |
        0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2069 |
        0xFE00..=0xFE0F | 0xFEFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_not_wide() {
        for cp in 0x00..0x7F {
            assert!(!is_wide(cp), "U+{:04X} should not be wide", cp);
        }
    }

    #[test]
    fn cjk_ranges_are_wide() {
        assert!(is_wide(0x1100));
        assert!(is_wide(0x115F));
        assert!(is_wide(0x2E80));
        assert!(is_wide(0x2F00));
        assert!(is_wide(0x303E));
        assert!(is_wide(0x3041));
        assert!(is_wide(0x33BF));
        assert!(is_wide(0x3400));
        assert!(is_wide(0x4DBF));
        assert!(is_wide(0x4E00));
        assert!(is_wide(0x9FFF));
        assert!(is_wide(0xA4CF));
        assert!(is_wide(0xA960));
        assert!(is_wide(0xA97C));
        assert!(is_wide(0xAC00));
        assert!(is_wide(0xBEEF));
        assert!(is_wide(0xD7A3));
    }

    #[test]
    fn emoji_ranges_are_wide() {
        assert!(is_wide(0x1F600));
        assert!(is_wide(0x1F9FF));
        assert!(is_wide(0x1FA00));
        assert!(is_wide(0x1FA6F));
        assert!(is_wide(0x1FA70));
        assert!(is_wide(0x1FAFF));
        assert!(is_wide(0x1F300));
        assert!(is_wide(0x1F0A0));
        assert!(is_wide(0x1F0FF));
        assert!(is_wide(0x1F000));
        assert!(is_wide(0x1F02F));
    }

    #[test]
    fn supplementary_cjk_are_wide() {
        assert!(is_wide(0x20000));
        assert!(is_wide(0x23456));
        assert!(is_wide(0x2FA1F));
    }

    #[test]
    fn latin_and_common_are_not_wide() {
        assert!(!is_wide(0x00BF));
        assert!(!is_wide(0x00FF));
        assert!(!is_wide(0x0100));
        assert!(!is_wide(0x0FFF));
        assert!(!is_wide(0x2000));
        assert!(!is_wide(0x200F));
        assert!(!is_wide(0x2100));
        assert!(!is_wide(0x2E7F));
        assert!(!is_wide(0x303F));
        assert!(!is_wide(0x3040));
    }

    #[test]
    fn wide_boundary_edges() {
        assert!(!is_wide(0x10FF));
        assert!(is_wide(0x1100));
        assert!(is_wide(0x115F));
        assert!(!is_wide(0x1160));

        assert!(!is_wide(0x2E7F));
        assert!(is_wide(0x2E80));
        assert!(is_wide(0x303E));
        assert!(!is_wide(0x303F));

        assert!(!is_wide(0xD7A4));
        assert!(is_wide(0xD7A3));
        assert!(is_wide(0xAC00));

        assert!(is_wide(0xF900));
        assert!(is_wide(0xFAFF));
        assert!(!is_wide(0xFB00));
    }

    #[test]
    fn combining_marks_are_zero_width() {
        assert!(is_zero_width(0x0300));
        assert!(is_zero_width(0x036F));
        assert!(is_zero_width(0x0483));
        assert!(is_zero_width(0x0489));
        assert!(is_zero_width(0x0591));
        assert!(is_zero_width(0x05BD));
        assert!(is_zero_width(0x0610));
        assert!(is_zero_width(0x064B));
        assert!(is_zero_width(0x065F));
        assert!(is_zero_width(0x0670));
        assert!(is_zero_width(0x06D6));
        assert!(is_zero_width(0x06ED));
        assert!(is_zero_width(0x0711));
        assert!(is_zero_width(0x0730));
        assert!(is_zero_width(0x074A));
    }

    #[test]
    fn format_characters_are_zero_width() {
        assert!(is_zero_width(0x200B));
        assert!(is_zero_width(0x200D));
        assert!(is_zero_width(0x200F));
        assert!(is_zero_width(0x2028));
        assert!(is_zero_width(0x202E));
        assert!(is_zero_width(0x2060));
        assert!(is_zero_width(0x2069));
    }

    #[test]
    fn variation_selectors_are_zero_width() {
        assert!(is_zero_width(0xFE00));
        assert!(is_zero_width(0xFE0F));
        assert!(is_zero_width(0xFEFF));
    }

    #[test]
    fn normal_chars_are_not_zero_width() {
        assert!(!is_zero_width(b'a' as u32));
        assert!(!is_zero_width(b' ' as u32));
        assert!(!is_zero_width(0x00B0));
        assert!(!is_zero_width(0x0370));
        assert!(!is_zero_width(0x048A));
        assert!(!is_zero_width(0x200A));
        assert!(!is_zero_width(0x2010));
        assert!(!is_zero_width(0x4E00));
    }

    #[test]
    fn zero_width_boundary_edges() {
        assert!(!is_zero_width(0x02FF));
        assert!(is_zero_width(0x0300));
        assert!(is_zero_width(0x036F));
        assert!(!is_zero_width(0x0370));

        assert!(!is_zero_width(0x0482));
        assert!(is_zero_width(0x0483));
        assert!(is_zero_width(0x0489));
        assert!(!is_zero_width(0x048A));

        assert!(!is_zero_width(0x0590));
        assert!(is_zero_width(0x0591));
        assert!(is_zero_width(0x05BD));
        assert!(!is_zero_width(0x05BE));

        assert!(is_zero_width(0xFE00));
        assert!(is_zero_width(0xFE0F));
        assert!(!is_zero_width(0xFE10));

        assert!(!is_zero_width(0x200A));
        assert!(is_zero_width(0x200B));
        assert!(is_zero_width(0x200F));
        assert!(!is_zero_width(0x2010));
    }
}
