/// Packed VT state transition table.
///
/// Based on Paul Williams' VT500 parser model.
/// Each entry encodes: high nibble = action, low nibble = next state.

// States
pub const GROUND: u8 = 0;
pub const ESCAPE: u8 = 1;
pub const ESCAPE_INTERMEDIATE: u8 = 2;
pub const CSI_ENTRY: u8 = 3;
pub const CSI_PARAM: u8 = 4;
pub const CSI_INTERMEDIATE: u8 = 5;
pub const CSI_IGNORE: u8 = 6;
pub const DCS_ENTRY: u8 = 7;
pub const DCS_PARAM: u8 = 8;
pub const DCS_INTERMEDIATE: u8 = 9;
pub const DCS_PASSTHROUGH: u8 = 10;
pub const DCS_IGNORE: u8 = 11;
pub const OSC_STRING: u8 = 12;
pub const SOS_PM_APC_STRING: u8 = 13;

pub const NUM_STATES: usize = 14;

// Actions
pub const ACTION_NONE: u8 = 0;
pub const ACTION_PRINT: u8 = 1;
pub const ACTION_EXECUTE: u8 = 2;
pub const ACTION_CLEAR: u8 = 3;
pub const ACTION_COLLECT: u8 = 4;
pub const ACTION_PARAM: u8 = 5;
pub const ACTION_ESC_DISPATCH: u8 = 6;
pub const ACTION_CSI_DISPATCH: u8 = 7;
pub const ACTION_HOOK: u8 = 8;
pub const ACTION_PUT: u8 = 9;
pub const ACTION_UNHOOK: u8 = 10;
pub const ACTION_OSC_START: u8 = 11;
pub const ACTION_OSC_PUT: u8 = 12;
pub const ACTION_OSC_END: u8 = 13;
pub const ACTION_IGNORE: u8 = 14;

/// Pack action + state into a single byte.
const fn pack(action: u8, state: u8) -> u8 {
    (action << 4) | state
}

/// Unpack: returns (action, next_state).
#[inline]
pub fn unpack(packed: u8) -> (u8, u8) {
    (packed >> 4, packed & 0x0F)
}

/// Classify a byte into one of 25 equivalence classes.
#[inline]
pub fn byte_class(byte: u8) -> usize {
    BYTE_CLASSES[byte as usize] as usize
}

// Byte equivalence classes
const BYTE_CLASSES: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = match i as u8 {
            0x00..=0x17 => 0,  // C0 controls (except ESC, CAN, SUB)
            0x18 => 1,         // CAN
            0x19 => 0,         // C0
            0x1A => 2,         // SUB
            0x1B => 3,         // ESC
            0x1C..=0x1F => 0,  // C0
            0x20..=0x2F => 4,  // Intermediates (space through /)
            0x30..=0x39 => 5,  // Digits
            0x3A => 6,         // Colon
            0x3B => 7,         // Semicolon
            0x3C..=0x3F => 8,  // < = > ?
            0x40..=0x5F => 9,  // @ through _  (uppercase + some punct)
            0x60..=0x7E => 10, // ` through ~  (lowercase + some punct)
            0x7F => 11,        // DEL
            0x80..=0x8F => 12, // C1 (8-bit)
            0x90 => 13,        // DCS
            0x91..=0x97 => 12, // C1
            0x98 => 14,        // SOS
            0x99..=0x9A => 12, // C1
            0x9B => 15,        // CSI
            0x9C => 16,        // ST
            0x9D => 17,        // OSC
            0x9E..=0x9F => 14, // PM, APC
            0xA0..=0xFF => 18, // High bytes (printable in some contexts)
        };
        i += 1;
    }
    t
};

const NUM_CLASSES: usize = 19;

/// State transition table: STATE_TABLE[state][byte_class] = packed(action, next_state)
pub static STATE_TABLE: [[u8; NUM_CLASSES]; NUM_STATES] = build_table();

const fn build_table() -> [[u8; NUM_CLASSES]; NUM_STATES] {
    let mut t = [[pack(ACTION_NONE, GROUND); NUM_CLASSES]; NUM_STATES];

    // ── GROUND state ──
    // C0 controls → execute, stay in ground
    t[GROUND as usize][0] = pack(ACTION_EXECUTE, GROUND);
    // CAN → execute, ground
    t[GROUND as usize][1] = pack(ACTION_EXECUTE, GROUND);
    // SUB → execute, ground
    t[GROUND as usize][2] = pack(ACTION_EXECUTE, GROUND);
    // ESC → clear, enter escape
    t[GROUND as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    // Intermediates (0x20-0x2F) → print (space is printable)
    t[GROUND as usize][4] = pack(ACTION_PRINT, GROUND);
    // Digits → print
    t[GROUND as usize][5] = pack(ACTION_PRINT, GROUND);
    // Colon → print
    t[GROUND as usize][6] = pack(ACTION_PRINT, GROUND);
    // Semicolon → print
    t[GROUND as usize][7] = pack(ACTION_PRINT, GROUND);
    // < = > ? → print
    t[GROUND as usize][8] = pack(ACTION_PRINT, GROUND);
    // @ through _ → print
    t[GROUND as usize][9] = pack(ACTION_PRINT, GROUND);
    // ` through ~ → print
    t[GROUND as usize][10] = pack(ACTION_PRINT, GROUND);
    // DEL → ignore
    t[GROUND as usize][11] = pack(ACTION_IGNORE, GROUND);
    // C1 → execute
    t[GROUND as usize][12] = pack(ACTION_EXECUTE, GROUND);
    // DCS → clear, enter DCS entry
    t[GROUND as usize][13] = pack(ACTION_CLEAR, DCS_ENTRY);
    // SOS/PM/APC → enter SOS string
    t[GROUND as usize][14] = pack(ACTION_NONE, SOS_PM_APC_STRING);
    // CSI → clear, enter CSI entry
    t[GROUND as usize][15] = pack(ACTION_CLEAR, CSI_ENTRY);
    // ST → ignore (no sequence to end)
    t[GROUND as usize][16] = pack(ACTION_IGNORE, GROUND);
    // OSC → enter OSC string
    t[GROUND as usize][17] = pack(ACTION_OSC_START, OSC_STRING);
    // High bytes → print
    t[GROUND as usize][18] = pack(ACTION_PRINT, GROUND);

    // ── ESCAPE state ──
    // C0 → execute, stay
    t[ESCAPE as usize][0] = pack(ACTION_EXECUTE, ESCAPE);
    t[ESCAPE as usize][1] = pack(ACTION_EXECUTE, GROUND); // CAN
    t[ESCAPE as usize][2] = pack(ACTION_EXECUTE, GROUND); // SUB
    t[ESCAPE as usize][3] = pack(ACTION_CLEAR, ESCAPE);   // ESC (re-enter)
    // Intermediates → collect, enter escape_intermediate
    t[ESCAPE as usize][4] = pack(ACTION_COLLECT, ESCAPE_INTERMEDIATE);
    // 0x30-0x4F → esc_dispatch, ground (most ESC sequences)
    t[ESCAPE as usize][5] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE as usize][6] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE as usize][7] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE as usize][8] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE as usize][9] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE as usize][10] = pack(ACTION_ESC_DISPATCH, GROUND);
    // DEL → ignore
    t[ESCAPE as usize][11] = pack(ACTION_IGNORE, ESCAPE);
    // High bytes in escape → ignore
    t[ESCAPE as usize][12] = pack(ACTION_EXECUTE, ESCAPE);
    t[ESCAPE as usize][13] = pack(ACTION_CLEAR, DCS_ENTRY);
    t[ESCAPE as usize][14] = pack(ACTION_NONE, SOS_PM_APC_STRING);
    t[ESCAPE as usize][15] = pack(ACTION_CLEAR, CSI_ENTRY);
    t[ESCAPE as usize][16] = pack(ACTION_IGNORE, GROUND);
    t[ESCAPE as usize][17] = pack(ACTION_OSC_START, OSC_STRING);
    t[ESCAPE as usize][18] = pack(ACTION_IGNORE, ESCAPE);

    // ── ESCAPE_INTERMEDIATE ──
    t[ESCAPE_INTERMEDIATE as usize][0] = pack(ACTION_EXECUTE, ESCAPE_INTERMEDIATE);
    t[ESCAPE_INTERMEDIATE as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[ESCAPE_INTERMEDIATE as usize][4] = pack(ACTION_COLLECT, ESCAPE_INTERMEDIATE);
    t[ESCAPE_INTERMEDIATE as usize][5] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][6] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][7] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][8] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][9] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][10] = pack(ACTION_ESC_DISPATCH, GROUND);
    t[ESCAPE_INTERMEDIATE as usize][11] = pack(ACTION_IGNORE, ESCAPE_INTERMEDIATE);

    // ── CSI_ENTRY ──
    t[CSI_ENTRY as usize][0] = pack(ACTION_EXECUTE, CSI_ENTRY);
    t[CSI_ENTRY as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_ENTRY as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_ENTRY as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[CSI_ENTRY as usize][4] = pack(ACTION_COLLECT, CSI_INTERMEDIATE);
    t[CSI_ENTRY as usize][5] = pack(ACTION_PARAM, CSI_PARAM);
    t[CSI_ENTRY as usize][6] = pack(ACTION_PARAM, CSI_PARAM); // colon
    t[CSI_ENTRY as usize][7] = pack(ACTION_PARAM, CSI_PARAM); // semicolon
    t[CSI_ENTRY as usize][8] = pack(ACTION_COLLECT, CSI_PARAM); // < = > ?  (private markers)
    t[CSI_ENTRY as usize][9] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_ENTRY as usize][10] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_ENTRY as usize][11] = pack(ACTION_IGNORE, CSI_ENTRY);

    // ── CSI_PARAM ──
    t[CSI_PARAM as usize][0] = pack(ACTION_EXECUTE, CSI_PARAM);
    t[CSI_PARAM as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_PARAM as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_PARAM as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[CSI_PARAM as usize][4] = pack(ACTION_COLLECT, CSI_INTERMEDIATE);
    t[CSI_PARAM as usize][5] = pack(ACTION_PARAM, CSI_PARAM);
    t[CSI_PARAM as usize][6] = pack(ACTION_PARAM, CSI_PARAM);
    t[CSI_PARAM as usize][7] = pack(ACTION_PARAM, CSI_PARAM);
    t[CSI_PARAM as usize][8] = pack(ACTION_NONE, CSI_IGNORE); // unexpected private marker
    t[CSI_PARAM as usize][9] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_PARAM as usize][10] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_PARAM as usize][11] = pack(ACTION_IGNORE, CSI_PARAM);

    // ── CSI_INTERMEDIATE ──
    t[CSI_INTERMEDIATE as usize][0] = pack(ACTION_EXECUTE, CSI_INTERMEDIATE);
    t[CSI_INTERMEDIATE as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_INTERMEDIATE as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_INTERMEDIATE as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[CSI_INTERMEDIATE as usize][4] = pack(ACTION_COLLECT, CSI_INTERMEDIATE);
    t[CSI_INTERMEDIATE as usize][5] = pack(ACTION_NONE, CSI_IGNORE);
    t[CSI_INTERMEDIATE as usize][6] = pack(ACTION_NONE, CSI_IGNORE);
    t[CSI_INTERMEDIATE as usize][7] = pack(ACTION_NONE, CSI_IGNORE);
    t[CSI_INTERMEDIATE as usize][8] = pack(ACTION_NONE, CSI_IGNORE);
    t[CSI_INTERMEDIATE as usize][9] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_INTERMEDIATE as usize][10] = pack(ACTION_CSI_DISPATCH, GROUND);
    t[CSI_INTERMEDIATE as usize][11] = pack(ACTION_IGNORE, CSI_INTERMEDIATE);

    // ── CSI_IGNORE ──
    t[CSI_IGNORE as usize][0] = pack(ACTION_EXECUTE, CSI_IGNORE);
    t[CSI_IGNORE as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_IGNORE as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[CSI_IGNORE as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[CSI_IGNORE as usize][9] = pack(ACTION_NONE, GROUND);
    t[CSI_IGNORE as usize][10] = pack(ACTION_NONE, GROUND);
    t[CSI_IGNORE as usize][11] = pack(ACTION_IGNORE, CSI_IGNORE);

    // ── DCS_ENTRY ──
    t[DCS_ENTRY as usize][0] = pack(ACTION_IGNORE, DCS_ENTRY);
    t[DCS_ENTRY as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_ENTRY as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_ENTRY as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[DCS_ENTRY as usize][4] = pack(ACTION_COLLECT, DCS_INTERMEDIATE);
    t[DCS_ENTRY as usize][5] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_ENTRY as usize][6] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_ENTRY as usize][7] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_ENTRY as usize][8] = pack(ACTION_COLLECT, DCS_PARAM);
    t[DCS_ENTRY as usize][9] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_ENTRY as usize][10] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_ENTRY as usize][11] = pack(ACTION_IGNORE, DCS_ENTRY);

    // ── DCS_PARAM ──
    t[DCS_PARAM as usize][0] = pack(ACTION_IGNORE, DCS_PARAM);
    t[DCS_PARAM as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_PARAM as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_PARAM as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[DCS_PARAM as usize][4] = pack(ACTION_COLLECT, DCS_INTERMEDIATE);
    t[DCS_PARAM as usize][5] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_PARAM as usize][6] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_PARAM as usize][7] = pack(ACTION_PARAM, DCS_PARAM);
    t[DCS_PARAM as usize][8] = pack(ACTION_NONE, DCS_IGNORE);
    t[DCS_PARAM as usize][9] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_PARAM as usize][10] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_PARAM as usize][11] = pack(ACTION_IGNORE, DCS_PARAM);

    // ── DCS_INTERMEDIATE ──
    t[DCS_INTERMEDIATE as usize][0] = pack(ACTION_IGNORE, DCS_INTERMEDIATE);
    t[DCS_INTERMEDIATE as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_INTERMEDIATE as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_INTERMEDIATE as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[DCS_INTERMEDIATE as usize][4] = pack(ACTION_COLLECT, DCS_INTERMEDIATE);
    t[DCS_INTERMEDIATE as usize][5] = pack(ACTION_NONE, DCS_IGNORE);
    t[DCS_INTERMEDIATE as usize][9] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_INTERMEDIATE as usize][10] = pack(ACTION_HOOK, DCS_PASSTHROUGH);
    t[DCS_INTERMEDIATE as usize][11] = pack(ACTION_IGNORE, DCS_INTERMEDIATE);

    // ── DCS_PASSTHROUGH ──
    t[DCS_PASSTHROUGH as usize][0] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][1] = pack(ACTION_UNHOOK, GROUND);
    t[DCS_PASSTHROUGH as usize][2] = pack(ACTION_UNHOOK, GROUND);
    t[DCS_PASSTHROUGH as usize][3] = pack(ACTION_UNHOOK, ESCAPE); // ESC in DCS ends it
    t[DCS_PASSTHROUGH as usize][4] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][5] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][6] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][7] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][8] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][9] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][10] = pack(ACTION_PUT, DCS_PASSTHROUGH);
    t[DCS_PASSTHROUGH as usize][11] = pack(ACTION_IGNORE, DCS_PASSTHROUGH);
    // ST (0x9C) ends DCS
    t[DCS_PASSTHROUGH as usize][16] = pack(ACTION_UNHOOK, GROUND);

    // ── DCS_IGNORE ──
    t[DCS_IGNORE as usize][0] = pack(ACTION_IGNORE, DCS_IGNORE);
    t[DCS_IGNORE as usize][1] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_IGNORE as usize][2] = pack(ACTION_EXECUTE, GROUND);
    t[DCS_IGNORE as usize][3] = pack(ACTION_CLEAR, ESCAPE);
    t[DCS_IGNORE as usize][16] = pack(ACTION_NONE, GROUND);

    // ── OSC_STRING ──
    t[OSC_STRING as usize][0] = pack(ACTION_IGNORE, OSC_STRING); // C0 (ignore most)
    t[OSC_STRING as usize][1] = pack(ACTION_OSC_END, GROUND); // CAN
    t[OSC_STRING as usize][2] = pack(ACTION_OSC_END, GROUND); // SUB
    t[OSC_STRING as usize][3] = pack(ACTION_OSC_END, ESCAPE); // ESC (might be ESC \)
    t[OSC_STRING as usize][4] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][5] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][6] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][7] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][8] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][9] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][10] = pack(ACTION_OSC_PUT, OSC_STRING);
    t[OSC_STRING as usize][11] = pack(ACTION_IGNORE, OSC_STRING);
    t[OSC_STRING as usize][16] = pack(ACTION_OSC_END, GROUND); // ST
    t[OSC_STRING as usize][18] = pack(ACTION_OSC_PUT, OSC_STRING); // high bytes

    // ── SOS_PM_APC_STRING ──
    t[SOS_PM_APC_STRING as usize][0] = pack(ACTION_IGNORE, SOS_PM_APC_STRING);
    t[SOS_PM_APC_STRING as usize][1] = pack(ACTION_NONE, GROUND);
    t[SOS_PM_APC_STRING as usize][2] = pack(ACTION_NONE, GROUND);
    t[SOS_PM_APC_STRING as usize][3] = pack(ACTION_NONE, ESCAPE);
    t[SOS_PM_APC_STRING as usize][16] = pack(ACTION_NONE, GROUND);

    t
}
