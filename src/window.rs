use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask,
    NSEventModifierFlags, NSEventType, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

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
    },
    Resized {
        w: u32,
        h: u32,
        scale: f64,
    },
    FocusIn,
    FocusOut,
    #[allow(dead_code)]
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
        unsafe { objc2::msg_send![super(obj), initWithFrame: frame] }
    }
}

// ── NativeWindow ──

pub struct NativeWindow {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    view: Retained<TtyView>,
    events: Vec<Event>,
    last_frame: NSRect,
    last_scale: f64,
    last_focused: bool,
    safe_area_top: u32,
    _mtm: MainThreadMarker,
}

impl NativeWindow {
    pub fn new() -> Self {
        let mtm = MainThreadMarker::new().expect("must be called from the main thread");

        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.finishLaunching();

        let screen = NSScreen::mainScreen(mtm).expect("no main screen");
        let frame = screen.frame();
        let scale = screen.backingScaleFactor();

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };

        // Hide titlebar chrome — content fills the entire window
        unsafe {
            let _: () = objc2::msg_send![&window, setTitlebarAppearsTransparent: true];
            // NSWindowTitleHidden = 1
            let _: () = objc2::msg_send![&window, setTitleVisibility: 1isize];
            // Black background to avoid bright flash during fullscreen transition
            let black: *const objc2::runtime::AnyObject = objc2::msg_send![
                objc2::runtime::AnyClass::get(c"NSColor").unwrap(),
                blackColor
            ];
            let _: () = objc2::msg_send![&window, setBackgroundColor: black];
        }

        let view = TtyView::new(frame, mtm);
        window.setContentView(Some(&view));
        window.setOpaque(true);

        // Enable native fullscreen
        window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
        window.makeKeyAndOrderFront(None);

        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        // Enter native fullscreen with suppressed animation
        unsafe {
            let ctx_cls = objc2::runtime::AnyClass::get(c"NSAnimationContext").unwrap();
            let _: () = objc2::msg_send![ctx_cls, beginGrouping];
            let ctx: *const objc2::runtime::AnyObject = objc2::msg_send![ctx_cls, currentContext];
            let _: () = objc2::msg_send![ctx, setDuration: 0.0f64];
            let _: () = objc2::msg_send![&window, toggleFullScreen: std::ptr::null::<objc2::runtime::AnyObject>()];
            let _: () = objc2::msg_send![ctx_cls, endGrouping];
        }

        // Detect safe area inset for notch (physical pixels)
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
            app,
            window,
            view,
            events: Vec::with_capacity(32),
            last_frame,
            last_scale: scale,
            last_focused: true,
            safe_area_top,
            _mtm: mtm,
        }
    }

    pub fn poll_events(&mut self) -> Vec<Event> {
        self.events.clear();

        loop {
            let event = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                None,
                unsafe { NSDefaultRunLoopMode },
                true,
            );

            let event = match event {
                Some(e) => e,
                None => break,
            };

            let event_type = event.r#type();

            if event_type == NSEventType::KeyDown {
                if let Some(ev) = self.translate_key_event(&event) {
                    self.events.push(ev);
                }
            } else if event_type == NSEventType::FlagsChanged {
                let mods = Modifiers::from_ns_flags(event.modifierFlags());
                self.events
                    .push(Event::ModifiersChanged { modifiers: mods });
            } else if event_type == NSEventType::LeftMouseDown {
                let (x, y) = self.mouse_position(&event);
                self.events.push(Event::MouseDown { x, y });
            } else if event_type == NSEventType::LeftMouseUp {
                let (x, y) = self.mouse_position(&event);
                self.events.push(Event::MouseUp { x, y });
            } else if event_type == NSEventType::LeftMouseDragged {
                let (x, y) = self.mouse_position(&event);
                self.events.push(Event::MouseDragged { x, y });
            } else if event_type == NSEventType::ScrollWheel {
                let (x, y) = self.mouse_position(&event);
                let delta_y = event.scrollingDeltaY();
                if delta_y != 0.0 {
                    self.events.push(Event::ScrollWheel { x, y, delta_y });
                }
            }

            self.app.sendEvent(&event);
        }

        // Check for resize / scale change
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
            self.events.push(Event::Resized {
                w: phys_w,
                h: phys_h,
                scale,
            });
        }

        // Check for focus change
        let focused = self.window.isKeyWindow();
        if focused != self.last_focused {
            self.last_focused = focused;
            self.events.push(if focused {
                Event::FocusIn
            } else {
                Event::FocusOut
            });
        }

        std::mem::take(&mut self.events)
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
