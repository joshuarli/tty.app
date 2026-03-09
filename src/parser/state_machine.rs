use crate::parser::perform::Perform;
use crate::parser::table::*;

const MAX_INTERMEDIATES: usize = 2;
const MAX_PARAMS: usize = 16;

/// Full VT state machine (Paul Williams model).
/// Handles all escape sequences that the CSI fast-path cannot.
pub struct StateMachine {
    state: u8,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_count: usize,
    params: [u16; MAX_PARAMS],
    param_count: usize,
    current_param: u32,
    osc_data: Vec<u8>,
    ignoring: bool,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: GROUND,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_count: 0,
            params: [0; MAX_PARAMS],
            param_count: 0,
            current_param: 0,
            osc_data: Vec::new(),
            ignoring: false,
        }
    }

    #[inline]
    pub fn is_ground(&self) -> bool {
        self.state == GROUND
    }

    /// Advance the state machine by one byte.
    pub fn advance<P: Perform>(&mut self, byte: u8, performer: &mut P) {
        let class = byte_class(byte);
        let packed = STATE_TABLE[self.state as usize][class];
        let (action, next_state) = unpack(packed);

        // Perform action
        match action {
            ACTION_PRINT => {
                performer.print(byte as char);
            }
            ACTION_EXECUTE => {
                performer.execute(byte);
            }
            ACTION_CLEAR => {
                self.intermediates = [0; MAX_INTERMEDIATES];
                self.intermediate_count = 0;
                self.params = [0; MAX_PARAMS];
                self.param_count = 0;
                self.current_param = 0;
                self.ignoring = false;
            }
            ACTION_COLLECT => {
                if self.intermediate_count < MAX_INTERMEDIATES {
                    self.intermediates[self.intermediate_count] = byte;
                    self.intermediate_count += 1;
                } else {
                    self.ignoring = true;
                }
            }
            ACTION_PARAM => {
                if byte == b';' {
                    if self.param_count < MAX_PARAMS {
                        self.params[self.param_count] =
                            self.current_param.min(u16::MAX as u32) as u16;
                        self.param_count += 1;
                    }
                    self.current_param = 0;
                } else if byte == b':' {
                    // Sub-parameter separator — treat like semicolon for now
                    if self.param_count < MAX_PARAMS {
                        self.params[self.param_count] =
                            self.current_param.min(u16::MAX as u32) as u16;
                        self.param_count += 1;
                    }
                    self.current_param = 0;
                } else if byte.is_ascii_digit() {
                    self.current_param = self
                        .current_param
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u32);
                }
            }
            ACTION_ESC_DISPATCH => {
                performer.esc_dispatch(&self.intermediates[..self.intermediate_count], byte);
            }
            ACTION_CSI_DISPATCH => {
                // Finalize last parameter
                if self.param_count < MAX_PARAMS {
                    self.params[self.param_count] = self.current_param.min(u16::MAX as u32) as u16;
                    self.param_count += 1;
                }
                performer.csi_dispatch(
                    &self.params[..self.param_count],
                    &self.intermediates[..self.intermediate_count],
                    self.ignoring,
                    byte,
                );
            }
            ACTION_HOOK => {
                // DCS hook — finalize params
                if self.param_count < MAX_PARAMS {
                    self.params[self.param_count] = self.current_param.min(u16::MAX as u32) as u16;
                    self.param_count += 1;
                }
                // We don't fully support DCS; just enter passthrough
            }
            ACTION_PUT => {
                // DCS passthrough data — ignore for now
            }
            ACTION_UNHOOK => {
                // DCS end — ignore for now
            }
            ACTION_OSC_START => {
                self.osc_data.clear();
            }
            ACTION_OSC_PUT => {
                self.osc_data.push(byte);
            }
            ACTION_OSC_END => {
                // Parse OSC: split on ';'
                let data = &self.osc_data;
                let mut parts: Vec<&[u8]> = Vec::new();
                let mut start = 0;
                for (i, &b) in data.iter().enumerate() {
                    if b == b';' {
                        parts.push(&data[start..i]);
                        start = i + 1;
                    }
                }
                parts.push(&data[start..]);
                performer.osc_dispatch(&parts);
                self.osc_data.clear();
            }
            ACTION_IGNORE | ACTION_NONE => {}
            _ => {}
        }

        self.state = next_state;
    }
}
