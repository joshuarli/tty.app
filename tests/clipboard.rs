use tty::clipboard::{clipboard_has_image, get_clipboard, set_clipboard, set_clipboard_image};

/// Minimal valid 1×1 red PNG (68 bytes).
const PNG_1PX: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // 8-bit RGB
    0xDE, //
    0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
    0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, //
    0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, //
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
    0xAE, 0x42, 0x60, 0x82, //
];

// These tests mutate the system clipboard, so they must not run in parallel.
// `cargo test -- --test-threads=1` or run this file alone.

#[test]
fn text_paste_works() {
    set_clipboard("hello");
    assert_eq!(get_clipboard().as_deref(), Some("hello"));
    assert!(!clipboard_has_image());
}

#[test]
fn image_on_clipboard_is_detected_as_png() {
    set_clipboard_image(PNG_1PX, "public.png");

    // No text should be returned.
    assert!(
        get_clipboard().is_none(),
        "get_clipboard() should return None when only image data is on the clipboard, \
         got: {:?}",
        get_clipboard()
    );

    // Image should be detected.
    assert!(
        clipboard_has_image(),
        "clipboard_has_image() should return true for PNG data"
    );
}

#[test]
fn image_on_clipboard_is_detected_as_tiff() {
    // Simulate a macOS screenshot (TIFF on clipboard). We don't need valid TIFF
    // bytes — clipboard_has_image() only checks that dataForType returns Some.
    set_clipboard_image(&[0xFF; 64], "public.tiff");

    assert!(
        get_clipboard().is_none(),
        "get_clipboard() should return None for TIFF-only clipboard"
    );

    assert!(
        clipboard_has_image(),
        "clipboard_has_image() should return true for TIFF data"
    );
}

#[test]
fn text_clipboard_is_not_image() {
    set_clipboard("just text");

    assert_eq!(get_clipboard().as_deref(), Some("just text"));
    assert!(
        !clipboard_has_image(),
        "clipboard_has_image() should be false when only text is present"
    );
}
