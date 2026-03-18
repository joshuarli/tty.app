use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent,
    NSEventModifierFlags, NSEventType, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

// ── Event types ──

#[derive(Debug, Clone)]
pub enum Event {
    KeyDown {
        key: Key,
        modifiers: Modifiers,
    },
    ModifiersChanged {
        modifiers: Modifiers,
    },
    MouseDown {
        x: f64,
        y: f64,
    },
    MouseUp {
        x: f64,
        y: f64,
    },
    MouseDragged {
        x: f64,
        y: f64,
    },
    ScrollWheel {
        x: f64,
        y: f64,
        delta_y: f64,
        precise: bool, // true = trackpad (points), false = mouse wheel (lines)
    },
    Resized {
        w: u32,
        h: u32,
        scale: f64,
    },
    FocusIn,
    FocusOut,
    Closed,
}

#[derive(Debug, Clone)]
pub enum Key {
    Character(String),
    Named(NamedKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Backspace,
    Tab,
    Enter,
    Escape,
    Space,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    const SHIFT: u8 = 1;
    const CONTROL: u8 = 2;
    const ALT: u8 = 4;
    const SUPER: u8 = 8;

    pub fn shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }
    pub fn control(self) -> bool {
        self.0 & Self::CONTROL != 0
    }
    pub fn alt(self) -> bool {
        self.0 & Self::ALT != 0
    }
    pub fn super_key(self) -> bool {
        self.0 & Self::SUPER != 0
    }

    fn from_ns_flags(flags: NSEventModifierFlags) -> Self {
        let mut m = 0u8;
        if flags.contains(NSEventModifierFlags::Shift) {
            m |= Self::SHIFT;
        }
        if flags.contains(NSEventModifierFlags::Control) {
            m |= Self::CONTROL;
        }
        if flags.contains(NSEventModifierFlags::Option) {
            m |= Self::ALT;
        }
        if flags.contains(NSEventModifierFlags::Command) {
            m |= Self::SUPER;
        }
        Self(m)
    }
}

// ── TtyView: minimal NSView subclass ──

define_class!(
    #[unsafe(super(NSView))]
    #[name = "TtyView"]
    struct TtyView;

    impl TtyView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }
    }
);

impl TtyView {
    fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let obj = Self::alloc(mtm).set_ivars(());
        // SAFETY: Calling NSView's initWithFrame: on a freshly allocated TtyView.
        // TtyView inherits from NSView, so the super init is valid.
        unsafe { objc2::msg_send![super(obj), initWithFrame: frame] }
    }
}

// ── App delegate (dock menu) ──

static NEW_WINDOW_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Check and reset the "new window requested" flag (set by dock menu).
pub fn new_window_requested() -> bool {
    NEW_WINDOW_REQUESTED.swap(false, Ordering::Relaxed)
}

/// Register the app delegate class and set it on the application.
/// Provides a dock menu with "New Window".
///
/// SAFETY: Must be called once, on the main thread, after NSApplication init.
unsafe fn setup_app_delegate(app: &NSApplication) {
    unsafe extern "C-unwind" fn dock_menu(
        this: &objc2::runtime::AnyObject,
        _cmd: objc2::runtime::Sel,
        _app: &objc2::runtime::AnyObject,
    ) -> *mut objc2::runtime::AnyObject {
        unsafe {
            let menu_cls = objc2::runtime::AnyClass::get(c"NSMenu").unwrap();
            let menu: *mut objc2::runtime::AnyObject = objc2::msg_send![menu_cls, new];

            let item_cls = objc2::runtime::AnyClass::get(c"NSMenuItem").unwrap();
            let item: *mut objc2::runtime::AnyObject = objc2::msg_send![item_cls, new];
            let title = NSString::from_str("New Window");
            let key = NSString::from_str("n");
            let _: () = objc2::msg_send![item, setTitle: &*title];
            let _: () = objc2::msg_send![item, setAction: objc2::sel!(newWindow:)];
            let _: () = objc2::msg_send![item, setKeyEquivalent: &*key];
            let _: () = objc2::msg_send![item, setTarget: this];
            let _: () = objc2::msg_send![menu, addItem: item];
            let _: () = objc2::msg_send![item, release];

            let menu: *mut objc2::runtime::AnyObject = objc2::msg_send![menu, autorelease];
            menu
        }
    }

    unsafe extern "C-unwind" fn new_window(
        _this: &objc2::runtime::AnyObject,
        _cmd: objc2::runtime::Sel,
        _sender: &objc2::runtime::AnyObject,
    ) {
        NEW_WINDOW_REQUESTED.store(true, Ordering::Relaxed);
    }

    let superclass = objc2::runtime::AnyClass::get(c"NSObject").unwrap();
    let mut builder = objc2::runtime::ClassBuilder::new(c"TtyAppDelegate", superclass)
        .expect("TtyAppDelegate class");
    unsafe {
        builder.add_method(
            objc2::sel!(applicationDockMenu:),
            dock_menu as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
        builder.add_method(
            objc2::sel!(newWindow:),
            new_window as unsafe extern "C-unwind" fn(_, _, _),
        );
    }
    let cls = builder.register();

    let delegate: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![cls, new] };
    let _: () = unsafe { objc2::msg_send![app, setDelegate: delegate] };
    // Intentionally leak — delegate lives for the app's lifetime
}

// ── Window delegate (close detection) ──

// Window pointers whose windowWillClose: fired this frame.
thread_local! {
    static CLOSED_WINDOWS: RefCell<Vec<*const objc2::runtime::AnyObject>> =
        const { RefCell::new(Vec::new()) };
}

/// Singleton window delegate shared by all windows.
static WINDOW_DELEGATE: AtomicPtr<objc2::runtime::AnyObject> =
    AtomicPtr::new(std::ptr::null_mut());

/// Get or create the shared window delegate instance.
fn window_delegate() -> *mut objc2::runtime::AnyObject {
    let ptr = WINDOW_DELEGATE.load(Ordering::Relaxed);
    if !ptr.is_null() {
        return ptr;
    }

    unsafe extern "C-unwind" fn window_will_close(
        _this: &objc2::runtime::AnyObject,
        _cmd: objc2::runtime::Sel,
        notification: &objc2::runtime::AnyObject,
    ) {
        let window: *const objc2::runtime::AnyObject =
            unsafe { objc2::msg_send![notification, object] };
        CLOSED_WINDOWS.with(|set| set.borrow_mut().push(window));
    }

    let superclass = objc2::runtime::AnyClass::get(c"NSObject").unwrap();
    let mut builder = objc2::runtime::ClassBuilder::new(c"TtyWindowDelegate", superclass)
        .expect("TtyWindowDelegate class");
    unsafe {
        builder.add_method(
            objc2::sel!(windowWillClose:),
            window_will_close as unsafe extern "C-unwind" fn(_, _, _),
        );
    }
    let cls = builder.register();

    let delegate: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![cls, new] };
    WINDOW_DELEGATE.store(delegate, Ordering::Relaxed);
    // Intentionally leak — shared delegate lives for the app's lifetime
    delegate
}

// ── NativeWindow ──

/// Initialize the NSApplication singleton. Call once at startup.
pub fn init_app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    // SAFETY: Called once, on the main thread, app is initialized above.
    unsafe { setup_app_delegate(&app) };

    app
}

pub struct NativeWindow {
    window: Retained<NSWindow>,
    view: Retained<TtyView>,
    last_frame: NSRect,
    last_scale: f64,
    last_focused: bool,
    safe_area_top: u32,
    closed: bool,
}

impl NativeWindow {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let screen = NSScreen::mainScreen(mtm).expect("no main screen");
        let frame = screen.frame();
        let scale = screen.backingScaleFactor();

        // SAFETY: Calling NSWindow's designated initializer with valid parameters.
        // mtm guarantees we are on the main thread as required by AppKit.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };

        // Hide titlebar chrome — content fills the entire window
        // SAFETY: These are standard NSWindow/NSColor messages. The window is valid
        // (just initialized above). NSColor.blackColor is a class method that always
        // succeeds. AnyClass::get(c"NSColor") is safe because NSColor is a built-in
        // AppKit class that is always loaded.
        unsafe {
            let _: () = objc2::msg_send![&window, setTitlebarAppearsTransparent: true];
            // NSWindowTitleHidden = 1
            let _: () = objc2::msg_send![&window, setTitleVisibility: 1isize];
            // Black background to avoid bright flash during fullscreen transition
            let black: *const objc2::runtime::AnyObject = objc2::msg_send![
                objc2::runtime::AnyClass::get(c"NSColor").expect("NSColor class"),
                blackColor
            ];
            let _: () = objc2::msg_send![&window, setBackgroundColor: black];
        }

        let view = TtyView::new(frame, mtm);
        window.setContentView(Some(&view));
        window.setOpaque(true);

        // Set window delegate for close detection (windowWillClose:)
        unsafe {
            let _: () = objc2::msg_send![&window, setDelegate: window_delegate()];
        }

        // Enable native fullscreen
        window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
        window.makeKeyAndOrderFront(None);

        // Enter native fullscreen with suppressed animation
        // SAFETY: NSAnimationContext is a built-in AppKit class. beginGrouping/endGrouping
        // bracket a zero-duration animation context so toggleFullScreen executes without
        // visible transition. The window is valid and on the main thread.
        unsafe {
            let ctx_cls = objc2::runtime::AnyClass::get(c"NSAnimationContext").expect("NSAnimationContext class");
            let _: () = objc2::msg_send![ctx_cls, beginGrouping];
            let ctx: *const objc2::runtime::AnyObject = objc2::msg_send![ctx_cls, currentContext];
            let _: () = objc2::msg_send![ctx, setDuration: 0.0f64];
            let _: () = objc2::msg_send![&window, toggleFullScreen: std::ptr::null::<objc2::runtime::AnyObject>()];
            let _: () = objc2::msg_send![ctx_cls, endGrouping];
        }

        // Detect safe area inset for notch (physical pixels)
        // SAFETY: safeAreaInsets is a valid NSScreen property (macOS 12+).
        // Returns NSEdgeInsets by value. screen is valid (from mainScreen above).
        let safe_area_top = unsafe {
            let insets: objc2_foundation::NSEdgeInsets = objc2::msg_send![&screen, safeAreaInsets];
            (insets.top * scale) as u32
        };

        let phys_w = (frame.size.width * scale) as u32;
        let phys_h = (frame.size.height * scale) as u32;
        let last_frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(phys_w as f64, phys_h as f64),
        );

        NativeWindow {
            window,
            view,
            last_frame,
            last_scale: scale,
            last_focused: true,
            safe_area_top,
            closed: false,
        }
    }

    /// Translate a single NSEvent into our Event enum.
    pub fn translate_ns_event(&self, event: &NSEvent) -> Option<Event> {
        let event_type = event.r#type();

        if event_type == NSEventType::KeyDown {
            return self.translate_key_event(event);
        } else if event_type == NSEventType::FlagsChanged {
            let mods = Modifiers::from_ns_flags(event.modifierFlags());
            return Some(Event::ModifiersChanged { modifiers: mods });
        } else if event_type == NSEventType::LeftMouseDown {
            let (x, y) = self.mouse_position(event);
            return Some(Event::MouseDown { x, y });
        } else if event_type == NSEventType::LeftMouseUp {
            let (x, y) = self.mouse_position(event);
            return Some(Event::MouseUp { x, y });
        } else if event_type == NSEventType::LeftMouseDragged {
            let (x, y) = self.mouse_position(event);
            return Some(Event::MouseDragged { x, y });
        } else if event_type == NSEventType::ScrollWheel {
            let (x, y) = self.mouse_position(event);
            let delta_y = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas();
            if delta_y != 0.0 {
                return Some(Event::ScrollWheel {
                    x,
                    y,
                    delta_y,
                    precise,
                });
            }
        }

        None
    }

    /// Check for close/resize/focus changes and push events.
    pub fn check_state_changes(&mut self, events: &mut Vec<Event>) {
        if self.closed {
            return;
        }

        // Detect window closed by user (red X button).
        // Uses windowWillClose: delegate callback rather than polling isVisible(),
        // which is unreliable during macOS Space transitions.
        let win_ptr = Retained::as_ptr(&self.window) as *const objc2::runtime::AnyObject;
        let was_closed = CLOSED_WINDOWS.with(|set| {
            let mut set = set.borrow_mut();
            if let Some(pos) = set.iter().position(|p| *p == win_ptr) {
                set.swap_remove(pos);
                true
            } else {
                false
            }
        });
        if was_closed {
            self.closed = true;
            events.push(Event::Closed);
            return;
        }

        let scale = self.window.backingScaleFactor();
        let frame = self.window.frame();
        let phys_w = (frame.size.width * scale) as u32;
        let phys_h = (frame.size.height * scale) as u32;
        let cur = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(phys_w as f64, phys_h as f64),
        );
        if cur.size.width != self.last_frame.size.width
            || cur.size.height != self.last_frame.size.height
            || scale != self.last_scale
        {
            self.last_frame = cur;
            self.last_scale = scale;
            events.push(Event::Resized {
                w: phys_w,
                h: phys_h,
                scale,
            });
        }

        let focused = self.window.isKeyWindow();
        if focused != self.last_focused {
            self.last_focused = focused;
            events.push(if focused {
                Event::FocusIn
            } else {
                Event::FocusOut
            });
        }
    }

    pub fn close(&self) {
        self.window.close();
    }

    /// Returns true if the given NSEvent belongs to this window.
    pub fn owns_ns_event(&self, event: &NSEvent, mtm: MainThreadMarker) -> bool {
        event
            .window(mtm)
            .as_ref()
            .is_some_and(|w| {
                Retained::as_ptr(w) == Retained::as_ptr(&self.window)
            })
    }

    pub fn view(&self) -> &NSView {
        &self.view
    }

    pub fn scale_factor(&self) -> f64 {
        self.window.backingScaleFactor()
    }

    pub fn physical_size(&self) -> (u32, u32) {
        let scale = self.window.backingScaleFactor();
        let frame = self.window.frame();
        (
            (frame.size.width * scale) as u32,
            (frame.size.height * scale) as u32,
        )
    }

    pub fn safe_area_top(&self) -> u32 {
        self.safe_area_top
    }

    pub fn set_title(&self, title: &str) {
        let ns_title = NSString::from_str(title);
        self.window.setTitle(&ns_title);
    }

    /// Convert NSEvent locationInWindow (bottom-left origin, points) to
    /// top-left origin physical pixels.
    fn mouse_position(&self, event: &NSEvent) -> (f64, f64) {
        let loc = event.locationInWindow();
        let scale = self.window.backingScaleFactor();
        let frame = self.window.frame();
        // Flip Y: AppKit is bottom-left origin, we want top-left
        let x = loc.x * scale;
        let y = (frame.size.height - loc.y) * scale;
        (x, y)
    }

    fn translate_key_event(&self, event: &NSEvent) -> Option<Event> {
        let key_code = event.keyCode();
        let flags = event.modifierFlags();
        let modifiers = Modifiers::from_ns_flags(flags);

        // Try named key from keyCode first
        if let Some(named) = keycode_to_named(key_code) {
            return Some(Event::KeyDown {
                key: Key::Named(named),
                modifiers,
            });
        }

        // Character key: use charactersIgnoringModifiers when Ctrl/Alt/Cmd held
        // (so input.rs gets the base letter), characters otherwise
        let has_cmd_ctrl_alt = flags.intersects(
            NSEventModifierFlags::Command
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Option,
        );

        let chars = if has_cmd_ctrl_alt {
            event.charactersIgnoringModifiers()
        } else {
            event.characters()
        };

        let s = chars?.to_string();
        if s.is_empty() {
            return None;
        }

        Some(Event::KeyDown {
            key: Key::Character(s),
            modifiers,
        })
    }
}

// ── Key code translation ──
// macOS virtual key codes → NamedKey

fn keycode_to_named(code: u16) -> Option<NamedKey> {
    match code {
        0x7E => Some(NamedKey::ArrowUp),
        0x7D => Some(NamedKey::ArrowDown),
        0x7B => Some(NamedKey::ArrowLeft),
        0x7C => Some(NamedKey::ArrowRight),
        0x73 => Some(NamedKey::Home),
        0x77 => Some(NamedKey::End),
        0x74 => Some(NamedKey::PageUp),
        0x79 => Some(NamedKey::PageDown),
        0x72 => Some(NamedKey::Insert), // Help/Insert
        0x75 => Some(NamedKey::Delete), // Forward delete
        0x33 => Some(NamedKey::Backspace),
        0x30 => Some(NamedKey::Tab),
        0x24 => Some(NamedKey::Enter),
        0x4C => Some(NamedKey::Enter), // Numpad enter
        0x35 => Some(NamedKey::Escape),
        0x31 => Some(NamedKey::Space),
        0x7A => Some(NamedKey::F1),
        0x78 => Some(NamedKey::F2),
        0x63 => Some(NamedKey::F3),
        0x76 => Some(NamedKey::F4),
        0x60 => Some(NamedKey::F5),
        0x61 => Some(NamedKey::F6),
        0x62 => Some(NamedKey::F7),
        0x64 => Some(NamedKey::F8),
        0x65 => Some(NamedKey::F9),
        0x6D => Some(NamedKey::F10),
        0x67 => Some(NamedKey::F11),
        0x6F => Some(NamedKey::F12),
        _ => None,
    }
}
