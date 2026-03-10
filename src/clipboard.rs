use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSData, NSString, ns_string};

pub fn set_clipboard(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let pasteboard_type = ns_string!("public.utf8-plain-text");
    let types = NSArray::from_slice(&[pasteboard_type]);
    // SAFETY: declareTypes_owner requires an unsafe call due to the optional owner
    // parameter (raw Objective-C pointer). Passing None is always safe.
    unsafe { pb.declareTypes_owner(&types, None) };
    let ns_text = NSString::from_str(text);
    pb.setString_forType(&ns_text, pasteboard_type);
}

pub fn get_clipboard() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    let pasteboard_type = ns_string!("public.utf8-plain-text");
    // Use dataForType to get raw bytes so that invalid UTF-8 (e.g. from pbcopy of
    // a file with lone high bytes) doesn't cause stringForType to return nil.
    let data: Retained<NSData> = pb.dataForType(pasteboard_type)?;
    Some(String::from_utf8_lossy(&data.to_vec()).into_owned())
}

pub fn clipboard_has_image() -> bool {
    let pb = NSPasteboard::generalPasteboard();
    let tiff_type = ns_string!("public.tiff");
    if pb.dataForType(tiff_type).is_some() {
        return true;
    }
    let png_type = ns_string!("public.png");
    pb.dataForType(png_type).is_some()
}

/// Place raw image data on the clipboard with the given UTI type.
/// Used by tests and by the paste-image path.
#[allow(dead_code)]
pub fn set_clipboard_image(data: &[u8], uti: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let pasteboard_type = NSString::from_str(uti);
    let types = NSArray::from_slice(&[&*pasteboard_type]);
    // SAFETY: Same as set_clipboard — None owner is always safe.
    unsafe { pb.declareTypes_owner(&types, None) };
    let ns_data = NSData::with_bytes(data);
    pb.setData_forType(Some(&ns_data), &pasteboard_type);
}
