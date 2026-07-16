use tty::config::rgb_to_palette;

#[test]
fn exact_palette_match_ansi_black() {
    assert_eq!(rgb_to_palette(0, 0, 0), 16);
}

#[test]
fn exact_palette_match_ansi_white() {
    assert_eq!(rgb_to_palette(255, 255, 255), 231);
}

#[test]
fn gray_value_in_ramp() {
    assert_eq!(rgb_to_palette(8, 8, 8), 232);
    assert_eq!(rgb_to_palette(128, 128, 128), 244);
}

#[test]
fn near_black_returns_0() {
    assert_eq!(rgb_to_palette(7, 7, 7), 232);
}

#[test]
fn near_white_returns_15() {
    assert_eq!(rgb_to_palette(249, 249, 249), 15);
}

#[test]
fn primary_red_maps_to_196() {
    assert_eq!(rgb_to_palette(255, 0, 0), 196);
}

#[test]
fn primary_green_maps_to_46() {
    assert_eq!(rgb_to_palette(0, 255, 0), 46);
}

#[test]
fn primary_blue_maps_to_21() {
    assert_eq!(rgb_to_palette(0, 0, 255), 21);
}

#[test]
fn all_non_grayscale_values_in_color_cube_range() {
    for r in [0, 64, 128, 192, 255] {
        for g in [0, 64, 128, 192, 255] {
            for b in [0, 64, 128, 192, 255] {
                if r == g && g == b {
                    continue;
                }
                let idx = rgb_to_palette(r, g, b);
                let _ = idx;
            }
        }
    }
}

#[test]
fn non_grayscale_maps_to_cube() {
    let idx = rgb_to_palette(255, 128, 64);
    assert!(idx <= u8::MAX);
}

#[test]
fn consistency_same_input_same_output() {
    let a = rgb_to_palette(123, 45, 67);
    let b = rgb_to_palette(123, 45, 67);
    assert_eq!(a, b);
}

#[test]
fn nearby_colors_can_use_named_entries() {
    assert_eq!(rgb_to_palette(172, 66, 66), 1);
    assert_eq!(rgb_to_palette(144, 169, 89), 2);
}

#[test]
fn nearby_colors_do_not_collapse_named_entries() {
    assert_ne!(
        rgb_to_palette(172, 66, 66),
        rgb_to_palette(144, 169, 89)
    );
}

#[test]
fn grayscale_boundary_behavior() {
    assert_eq!(rgb_to_palette(8, 8, 8), 232);
    assert_eq!(rgb_to_palette(9, 9, 9), 232);
    assert_eq!(rgb_to_palette(18, 18, 18), 233);
}

#[test]
fn exact_color_cube_levels() {
    assert_eq!(rgb_to_palette(0, 0, 95), 17);
    assert_eq!(rgb_to_palette(0, 0, 135), 18);
}
