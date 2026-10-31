//! Streaming VT recognizer derived from Foot 1.27.0 `vt.c`, `vt.h`, and
//! parser storage in `terminal.h` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`.
//!
//! Recognition remains byte-streaming and chunk-independent. CSI storage is
//! fixed at Foot's 16 parameters and 16 subparameters. OSC and DCS collection
//! is intentionally bounded; overflow consumes through the terminator without
//! desynchronizing subsequent input.

use std::{fmt, marker::PhantomData};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};

const PARAM_LIMIT: usize = 16;
const SUBPARAM_LIMIT: usize = 16;
const INTERMEDIATE_LIMIT: usize = 4;
const MAX_CHECKPOINT_PARSER_STRING_BYTES: usize = 1024 * 1024;
/// How much one OSC 52 string may collect (Plan 0074 M2): `52;`, its targets,
/// and base64 for [`crate::OSC52_MAX_BYTES`]. Only a string that begins `52;`
/// is given it, so every other OSC keeps the configured limit.
const OSC52_STRING_LIMIT: usize = 64 + crate::OSC52_MAX_BASE64_BYTES;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Param {
    value: u32,
    present: bool,
    subparams: [u32; SUBPARAM_LIMIT],
    subparam_present: [bool; SUBPARAM_LIMIT],
    subparam_count: usize,
}

impl Param {
    pub(crate) fn value(self, default: u32, zero_is_default: bool) -> u32 {
        if !self.present || (zero_is_default && self.value == 0) {
            default
        } else {
            self.value & 0x7fff_ffff
        }
    }

    pub(crate) fn subparam(self, index: usize) -> Option<u32> {
        (index < self.subparam_count && self.subparam_present[index])
            .then_some(self.subparams[index] & 0x7fff_ffff)
    }

    pub(crate) const fn subparam_count(self) -> usize {
        self.subparam_count
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Params {
    values: [Param; PARAM_LIMIT],
    count: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            values: [Param::default(); PARAM_LIMIT],
            count: 0,
        }
    }
}

impl Params {
    pub(crate) fn get(&self, index: usize) -> Param {
        if index < self.count {
            self.values[index]
        } else {
            Param::default()
        }
    }

    pub(crate) const fn count(&self) -> usize {
        self.count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StringTerminator {
    Bell,
    StringTerminator,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Print(char),
    Execute(u8),
    Esc {
        intermediates: [u8; INTERMEDIATE_LIMIT],
        intermediate_count: usize,
        final_byte: u8,
    },
    Csi {
        private: Option<u8>,
        intermediates: [u8; INTERMEDIATE_LIMIT],
        intermediate_count: usize,
        params: Box<Params>,
        final_byte: u8,
    },
    Osc(Vec<u8>, StringTerminator),
    Dcs(Vec<u8>),
    SixelBegin(Box<Params>),
    SixelData(u8),
    SixelEnd,
    SixelAbort,
    KittyBegin(Vec<u8>, bool),
    KittyCommand(Vec<u8>, bool),
    KittyData(u8),
    KittyEnd,
    KittyAbort,
    ItermBegin(Vec<u8>, bool),
    ItermData(u8),
    ItermEnd,
    ItermAbort,
    StringTruncated(&'static str),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    OscEscape,
    ItermControl,
    ItermControlEscape,
    ItermPayload,
    ItermPayloadEscape,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    DcsEscape,
    SosPmApcString,
    SosPmApcEscape,
    ApcPrefix,
    KittyControl,
    KittyControlEscape,
    KittyPayload,
    KittyPayloadEscape,
    Utf8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Parser {
    state: State,
    private: Option<u8>,
    intermediates: [u8; INTERMEDIATE_LIMIT],
    intermediate_count: usize,
    params: Params,
    current_param: usize,
    current_subparam: Option<usize>,
    parameter_overflow: bool,
    #[serde(deserialize_with = "deserialize_parser_string")]
    string: Vec<u8>,
    string_limit: usize,
    string_truncated: bool,
    osc_limit: usize,
    dcs_limit: usize,
    sixel_streaming: bool,
    utf8_value: u32,
    utf8_minimum: u32,
    utf8_remaining: u8,
    string_utf8_remaining: u8,
}

impl Parser {
    pub(crate) fn new(osc_limit: usize, dcs_limit: usize) -> Self {
        Self {
            state: State::Ground,
            private: None,
            intermediates: [0; INTERMEDIATE_LIMIT],
            intermediate_count: 0,
            params: Params::default(),
            current_param: 0,
            current_subparam: None,
            parameter_overflow: false,
            string: Vec::new(),
            string_limit: 0,
            string_truncated: false,
            osc_limit,
            dcs_limit,
            sixel_streaming: false,
            utf8_value: 0,
            utf8_minimum: 0,
            utf8_remaining: 0,
            string_utf8_remaining: 0,
        }
    }

    /// This parser as a checkpoint carries it. Only an OSC 52 string may hold
    /// more than the configured OSC limit. One still within it, such as a
    /// query, is carried whole; a longer one is carried as an overflowed OSC,
    /// so after a restore the rest of it is consumed and dropped. A
    /// checkpoint thus keeps ordinary OSC bounds and an older daemon still
    /// accepts it.
    pub(crate) fn checkpoint_copy(&self) -> Self {
        if !matches!(self.state, State::OscString | State::OscEscape) {
            // A finished string's limit never outlives it; this is a guard.
            return Self {
                string_limit: self.string_limit.min(self.normal_string_limit()),
                ..self.clone()
            };
        }
        if self.string_limit <= self.osc_limit {
            return self.clone();
        }
        if self.string.len() <= self.osc_limit {
            return Self {
                string_limit: self.osc_limit,
                ..self.clone()
            };
        }
        Self {
            string_limit: self.osc_limit,
            string_truncated: true,
            string_utf8_remaining: 0,
            ..self.clone_without_string()
        }
    }

    /// The largest string limit a checkpoint accepts.
    fn normal_string_limit(&self) -> usize {
        self.osc_limit.max(self.dcs_limit).max(1024)
    }

    fn clone_without_string(&self) -> Self {
        Self {
            state: self.state,
            private: self.private,
            intermediates: self.intermediates,
            intermediate_count: self.intermediate_count,
            params: self.params,
            current_param: self.current_param,
            current_subparam: self.current_subparam,
            parameter_overflow: self.parameter_overflow,
            string: Vec::new(),
            string_limit: self.string_limit,
            string_truncated: self.string_truncated,
            osc_limit: self.osc_limit,
            dcs_limit: self.dcs_limit,
            sixel_streaming: self.sixel_streaming,
            utf8_value: self.utf8_value,
            utf8_minimum: self.utf8_minimum,
            utf8_remaining: self.utf8_remaining,
            string_utf8_remaining: self.string_utf8_remaining,
        }
    }

    pub(crate) fn checkpoint_is_valid(&self, osc_limit: usize, dcs_limit: usize) -> bool {
        if self.osc_limit != osc_limit
            || self.dcs_limit != dcs_limit
            || self.intermediate_count > INTERMEDIATE_LIMIT
            || self.intermediates[self.intermediate_count..]
                .iter()
                .any(|byte| *byte != 0)
            || self.intermediates[..self.intermediate_count]
                .iter()
                .any(|byte| !(0x20..=0x2f).contains(byte))
            || self.params.count > PARAM_LIMIT
            || self.current_param >= PARAM_LIMIT
            || self
                .current_subparam
                .is_some_and(|index| index > SUBPARAM_LIMIT)
            || self
                .private
                .is_some_and(|byte| !(0x3c..=0x3f).contains(&byte))
            || self.string.len() > self.string_limit
            || self.string.len() > MAX_CHECKPOINT_PARSER_STRING_BYTES
            || self.string_limit > osc_limit.max(dcs_limit).max(1024)
            || self.sixel_streaming
            || self.utf8_remaining > 3
            || self.string_utf8_remaining > 3
            || (self.state == State::Utf8 && self.utf8_remaining == 0)
        {
            return false;
        }
        for (index, param) in self.params.values.iter().enumerate() {
            if param.subparam_count > SUBPARAM_LIMIT
                || (!param.present && param.value != 0)
                || param.subparams[param.subparam_count..]
                    .iter()
                    .any(|value| *value != 0)
                || param.subparam_present[param.subparam_count..]
                    .iter()
                    .any(|present| *present)
                || (index >= self.params.count && *param != Param::default())
            {
                return false;
            }
        }
        true
    }

    /// Returns an action and whether the same byte must be reprocessed.
    pub(crate) fn feed(&mut self, byte: u8) -> (Option<Action>, bool) {
        if self.state == State::Utf8 {
            return self.feed_utf8(byte);
        }

        if matches!(byte, 0x18 | 0x1a) {
            let action = if self.sixel_streaming {
                Action::SixelAbort
            } else if matches!(
                self.state,
                State::ItermControl
                    | State::ItermControlEscape
                    | State::ItermPayload
                    | State::ItermPayloadEscape
            ) {
                Action::ItermAbort
            } else {
                Action::KittyAbort
            };
            self.state = State::Ground;
            self.sixel_streaming = false;
            self.end_string();
            self.clear_sequence();
            return (Some(action), false);
        }
        if byte == 0x1b
            && !matches!(
                self.state,
                State::OscString
                    | State::OscEscape
                    | State::ItermControl
                    | State::ItermControlEscape
                    | State::ItermPayload
                    | State::ItermPayloadEscape
                    | State::DcsPassthrough
                    | State::DcsEscape
                    | State::SosPmApcString
                    | State::SosPmApcEscape
                    | State::ApcPrefix
                    | State::KittyControl
                    | State::KittyControlEscape
                    | State::KittyPayload
                    | State::KittyPayloadEscape
            )
        {
            self.clear_sequence();
            self.state = State::Escape;
            return (None, false);
        }

        match self.state {
            State::Ground => self.feed_ground(byte),
            State::Escape | State::EscapeIntermediate => self.feed_escape(byte),
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => {
                self.feed_csi(byte)
            }
            State::OscString
            | State::OscEscape
            | State::ItermControl
            | State::ItermControlEscape
            | State::ItermPayload
            | State::ItermPayloadEscape => self.feed_osc(byte),
            State::DcsEntry
            | State::DcsParam
            | State::DcsIntermediate
            | State::DcsIgnore
            | State::DcsPassthrough
            | State::DcsEscape => self.feed_dcs(byte),
            State::SosPmApcString | State::SosPmApcEscape => self.feed_ignored_string(byte),
            State::ApcPrefix
            | State::KittyControl
            | State::KittyControlEscape
            | State::KittyPayload
            | State::KittyPayloadEscape => self.feed_apc(byte),
            State::Utf8 => unreachable!(),
        }
    }

    fn feed_ground(&mut self, byte: u8) -> (Option<Action>, bool) {
        match byte {
            0x00..=0x1f => (Some(Action::Execute(byte)), false),
            0x20..=0x7e => (Some(Action::Print(char::from(byte))), false),
            0xc2..=0xdf => {
                self.start_utf8(u32::from(byte & 0x1f), 1, 0x80);
                (None, false)
            }
            0xe0..=0xef => {
                self.start_utf8(u32::from(byte & 0x0f), 2, 0x800);
                (None, false)
            }
            0xf0..=0xf4 => {
                self.start_utf8(u32::from(byte & 0x07), 3, 0x1_0000);
                (None, false)
            }
            0x9d => {
                self.start_string(State::OscString, self.osc_limit);
                (None, false)
            }
            0x9f => {
                self.state = State::ApcPrefix;
                (None, false)
            }
            _ => (None, false),
        }
    }

    fn feed_escape(&mut self, byte: u8) -> (Option<Action>, bool) {
        if byte == 0x7f {
            return (None, false);
        }
        match byte {
            0x00..=0x1f => (Some(Action::Execute(byte)), false),
            b'[' if self.state == State::Escape => {
                self.clear_sequence();
                self.state = State::CsiEntry;
                (None, false)
            }
            b']' if self.state == State::Escape => {
                self.start_string(State::OscString, self.osc_limit);
                (None, false)
            }
            b'P' if self.state == State::Escape => {
                self.clear_sequence();
                self.state = State::DcsEntry;
                (None, false)
            }
            b'X' | b'^' if self.state == State::Escape => {
                self.state = State::SosPmApcString;
                (None, false)
            }
            b'_' if self.state == State::Escape => {
                self.state = State::ApcPrefix;
                (None, false)
            }
            0x20..=0x2f => {
                self.collect_intermediate(byte);
                self.state = State::EscapeIntermediate;
                (None, false)
            }
            0x30..=0x7e => {
                let action = Action::Esc {
                    intermediates: self.intermediates,
                    intermediate_count: self.intermediate_count,
                    final_byte: byte,
                };
                self.state = State::Ground;
                (Some(action), false)
            }
            _ => {
                self.state = State::Ground;
                (None, false)
            }
        }
    }

    fn feed_csi(&mut self, byte: u8) -> (Option<Action>, bool) {
        if byte == 0x7f {
            return (None, false);
        }
        if byte <= 0x1f {
            return (Some(Action::Execute(byte)), false);
        }
        if self.state == State::CsiIgnore {
            if (0x40..=0x7e).contains(&byte) {
                self.state = State::Ground;
            }
            return (None, false);
        }
        if self.state == State::CsiIntermediate && (0x30..=0x3f).contains(&byte) {
            self.state = State::CsiIgnore;
            return (None, false);
        }
        match byte {
            0x30..=0x39 => {
                self.collect_digit(byte - b'0');
                self.state = State::CsiParam;
                (None, false)
            }
            b';' => {
                self.next_param();
                self.state = if self.parameter_overflow {
                    State::CsiIgnore
                } else {
                    State::CsiParam
                };
                (None, false)
            }
            b':' => {
                self.next_subparam();
                self.state = State::CsiParam;
                (None, false)
            }
            0x3c..=0x3f if self.state == State::CsiEntry && self.private.is_none() => {
                self.private = Some(byte);
                (None, false)
            }
            0x20..=0x2f => {
                self.collect_intermediate(byte);
                self.state = State::CsiIntermediate;
                (None, false)
            }
            0x40..=0x7e => {
                self.finish_params();
                let action = Action::Csi {
                    private: self.private,
                    intermediates: self.intermediates,
                    intermediate_count: self.intermediate_count,
                    params: Box::new(self.params),
                    final_byte: byte,
                };
                self.state = State::Ground;
                (Some(action), false)
            }
            _ => {
                self.state = State::CsiIgnore;
                (None, false)
            }
        }
    }

    fn feed_osc(&mut self, byte: u8) -> (Option<Action>, bool) {
        match self.state {
            State::OscString => match byte {
                0x07 => self.finish_string(true, StringTerminator::Bell),
                0x1b => {
                    self.state = State::OscEscape;
                    (None, false)
                }
                // 0x9c is also a UTF-8 continuation byte (for example in U+4F5C);
                // it is C1 ST only outside a multibyte sequence.
                0x9c if self.string_utf8_remaining == 0 => {
                    self.finish_string(true, StringTerminator::StringTerminator)
                }
                0x00..=0x1f | 0x7f => (None, false),
                _ => {
                    self.string_utf8_remaining = match byte {
                        0x80..=0xbf => self.string_utf8_remaining.saturating_sub(1),
                        0xc2..=0xdf => 1,
                        0xe0..=0xef => 2,
                        0xf0..=0xf4 => 3,
                        _ => 0,
                    };
                    // An OSC 52 string grows past the OSC limit when it reaches
                    // it, also one a checkpoint restored.
                    if self.string.len() == self.string_limit
                        && self.string_limit < OSC52_STRING_LIMIT
                        && self.string.starts_with(b"52;")
                    {
                        self.string_limit = OSC52_STRING_LIMIT;
                    }
                    self.push_string(byte);
                    if self.string == b"1337;File=" {
                        self.start_string(State::ItermControl, 1024);
                    }
                    (None, false)
                }
            },
            State::OscEscape => {
                let result = self.finish_string(true, StringTerminator::StringTerminator);
                if byte == b'\\' {
                    result
                } else {
                    self.state = State::Escape;
                    (result.0, true)
                }
            }
            State::ItermControl => match byte {
                b':' => {
                    let control = std::mem::take(&mut self.string);
                    let truncated = std::mem::take(&mut self.string_truncated);
                    self.state = State::ItermPayload;
                    (Some(Action::ItermBegin(control, truncated)), false)
                }
                0x07 | 0x9c => {
                    self.state = State::Ground;
                    self.string.clear();
                    (Some(Action::ItermAbort), false)
                }
                0x1b => {
                    self.state = State::ItermControlEscape;
                    (None, false)
                }
                _ => {
                    self.push_string(byte);
                    (None, false)
                }
            },
            State::ItermPayload => match byte {
                0x07 | 0x9c => {
                    self.state = State::Ground;
                    (Some(Action::ItermEnd), false)
                }
                0x1b => {
                    self.state = State::ItermPayloadEscape;
                    (None, false)
                }
                _ => (Some(Action::ItermData(byte)), false),
            },
            State::ItermControlEscape => {
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::Escape
                };
                self.string.clear();
                (Some(Action::ItermAbort), byte != b'\\')
            }
            State::ItermPayloadEscape => {
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::Escape
                };
                (
                    Some(if byte == b'\\' {
                        Action::ItermEnd
                    } else {
                        Action::ItermAbort
                    }),
                    byte != b'\\',
                )
            }
            _ => unreachable!(),
        }
    }

    fn feed_dcs(&mut self, byte: u8) -> (Option<Action>, bool) {
        match self.state {
            State::DcsPassthrough => match byte {
                0x1b => {
                    self.state = State::DcsEscape;
                    (None, false)
                }
                0x7f => (None, false),
                0x80..=0x9f => self.finish_dcs(),
                _ if self.sixel_streaming => (Some(Action::SixelData(byte)), false),
                _ => {
                    self.push_string(byte);
                    (None, false)
                }
            },
            State::DcsEscape => {
                if byte == b'\\' {
                    self.finish_dcs()
                } else {
                    let action = self.sixel_streaming.then_some(Action::SixelAbort);
                    self.sixel_streaming = false;
                    self.state = State::Escape;
                    self.clear_sequence();
                    (action, true)
                }
            }
            State::DcsIgnore => {
                if byte == 0x1b {
                    self.state = State::DcsEscape;
                }
                (None, false)
            }
            _ => {
                if byte <= 0x1f {
                    return (None, false);
                }
                match byte {
                    0x30..=0x39 => {
                        self.collect_digit(byte - b'0');
                        self.state = State::DcsParam;
                    }
                    b';' => {
                        self.next_param();
                        self.state = if self.parameter_overflow {
                            State::DcsIgnore
                        } else {
                            State::DcsParam
                        };
                    }
                    b':' => {
                        self.next_subparam();
                        self.state = State::DcsParam;
                    }
                    0x3c..=0x3f if self.state == State::DcsEntry => self.private = Some(byte),
                    0x20..=0x2f => {
                        self.collect_intermediate(byte);
                        self.state = State::DcsIntermediate;
                    }
                    0x40..=0x7e => {
                        self.finish_params();
                        if byte == b'q' && self.private.is_none() && self.intermediate_count == 0 {
                            self.state = State::DcsPassthrough;
                            self.sixel_streaming = true;
                            return (Some(Action::SixelBegin(Box::new(self.params))), false);
                        }
                        self.start_string(State::DcsPassthrough, self.dcs_limit);
                    }
                    _ => self.state = State::DcsIgnore,
                }
                (None, false)
            }
        }
    }

    fn feed_apc(&mut self, byte: u8) -> (Option<Action>, bool) {
        match self.state {
            State::ApcPrefix => {
                if byte == b'G' {
                    self.start_string(State::KittyControl, 1024);
                } else if byte == 0x1b {
                    self.state = State::SosPmApcEscape;
                } else if byte == 0x9c {
                    self.state = State::Ground;
                } else {
                    self.state = State::SosPmApcString;
                }
                (None, false)
            }
            State::KittyControl => match byte {
                b';' => {
                    let control = std::mem::take(&mut self.string);
                    let truncated = std::mem::take(&mut self.string_truncated);
                    self.state = State::KittyPayload;
                    (Some(Action::KittyBegin(control, truncated)), false)
                }
                0x1b => {
                    self.state = State::KittyControlEscape;
                    (None, false)
                }
                0x9c => self.finish_kitty_command(),
                _ => {
                    self.push_string(byte);
                    (None, false)
                }
            },
            State::KittyPayload => match byte {
                0x1b => {
                    self.state = State::KittyPayloadEscape;
                    (None, false)
                }
                0x9c => {
                    self.state = State::Ground;
                    (Some(Action::KittyEnd), false)
                }
                _ => (Some(Action::KittyData(byte)), false),
            },
            State::KittyControlEscape => {
                if byte == b'\\' {
                    self.finish_kitty_command()
                } else {
                    self.state = State::Escape;
                    self.string.clear();
                    (Some(Action::KittyAbort), true)
                }
            }
            State::KittyPayloadEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                    (Some(Action::KittyEnd), false)
                } else {
                    self.state = State::Escape;
                    (Some(Action::KittyAbort), true)
                }
            }
            _ => unreachable!(),
        }
    }

    fn finish_kitty_command(&mut self) -> (Option<Action>, bool) {
        self.state = State::Ground;
        let control = std::mem::take(&mut self.string);
        let truncated = std::mem::take(&mut self.string_truncated);
        (Some(Action::KittyCommand(control, truncated)), false)
    }

    fn feed_ignored_string(&mut self, byte: u8) -> (Option<Action>, bool) {
        if byte == 0x9c {
            self.state = State::Ground;
        } else if self.state == State::SosPmApcEscape {
            if byte == b'\\' {
                self.state = State::Ground;
                return (None, false);
            }
            self.state = State::SosPmApcString;
        } else if byte == 0x1b {
            self.state = State::SosPmApcEscape;
        }
        (None, false)
    }

    fn feed_utf8(&mut self, byte: u8) -> (Option<Action>, bool) {
        if byte & 0xc0 != 0x80 {
            self.state = State::Ground;
            return (Some(Action::Print('\u{fffd}')), true);
        }
        self.utf8_value = (self.utf8_value << 6) | u32::from(byte & 0x3f);
        self.utf8_remaining -= 1;
        if self.utf8_remaining > 0 {
            return (None, false);
        }
        self.state = State::Ground;
        let value = self.utf8_value;
        if value < self.utf8_minimum || value > 0x10_ffff || (0xd800..=0xdfff).contains(&value) {
            return (Some(Action::Print('\u{fffd}')), false);
        }
        (char::from_u32(value).map(Action::Print), false)
    }

    fn start_utf8(&mut self, value: u32, remaining: u8, minimum: u32) {
        self.state = State::Utf8;
        self.utf8_value = value;
        self.utf8_remaining = remaining;
        self.utf8_minimum = minimum;
    }

    fn start_string(&mut self, state: State, limit: usize) {
        self.state = state;
        self.free_string();
        self.string_limit = limit;
        self.string_truncated = false;
        self.string_utf8_remaining = 0;
    }

    fn push_string(&mut self, byte: u8) {
        if self.string.len() < self.string_limit {
            self.string.push(byte);
        } else {
            self.string_truncated = true;
        }
    }

    /// Empties the string buffer. One grown past the configured limits, as
    /// by an OSC 52 set, is freed rather than kept for the next string.
    fn free_string(&mut self) {
        if self.string.capacity() > self.osc_limit.max(self.dcs_limit) {
            self.string = Vec::new();
        } else {
            self.string.clear();
        }
    }

    /// Ends a string: frees its buffer, and drops an OSC 52 limit, which a
    /// checkpoint outside that string would refuse.
    fn end_string(&mut self) {
        self.free_string();
        self.string_limit = self.string_limit.min(self.normal_string_limit());
    }

    fn finish_string(&mut self, osc: bool, terminator: StringTerminator) -> (Option<Action>, bool) {
        self.state = State::Ground;
        let payload = std::mem::take(&mut self.string);
        self.end_string();
        if self.string_truncated {
            return (
                Some(Action::StringTruncated(if osc { "OSC" } else { "DCS" })),
                false,
            );
        }
        (Some(Action::Osc(payload, terminator)), false)
    }

    fn finish_dcs(&mut self) -> (Option<Action>, bool) {
        self.state = State::Ground;
        if self.sixel_streaming {
            self.sixel_streaming = false;
            self.clear_sequence();
            return (Some(Action::SixelEnd), false);
        }
        if self.string_truncated {
            self.end_string();
            (Some(Action::StringTruncated("DCS")), false)
        } else {
            (Some(Action::Dcs(std::mem::take(&mut self.string))), false)
        }
    }

    fn clear_sequence(&mut self) {
        self.private = None;
        self.intermediates = [0; INTERMEDIATE_LIMIT];
        self.intermediate_count = 0;
        self.params = Params::default();
        self.current_param = 0;
        self.current_subparam = None;
        self.parameter_overflow = false;
    }

    fn collect_intermediate(&mut self, byte: u8) {
        if self.intermediate_count < INTERMEDIATE_LIMIT {
            self.intermediates[self.intermediate_count] = byte;
            self.intermediate_count += 1;
        }
    }

    fn collect_digit(&mut self, digit: u8) {
        if self.current_param >= PARAM_LIMIT {
            return;
        }
        self.params.count = self.params.count.max(self.current_param + 1);
        let param = &mut self.params.values[self.current_param];
        if let Some(subparam) = self.current_subparam {
            if subparam < SUBPARAM_LIMIT {
                param.subparam_count = param.subparam_count.max(subparam + 1);
                param.subparam_present[subparam] = true;
                param.subparams[subparam] = param.subparams[subparam]
                    .wrapping_mul(10)
                    .wrapping_add(u32::from(digit));
            }
        } else {
            param.present = true;
            param.value = param.value.wrapping_mul(10).wrapping_add(u32::from(digit));
        }
    }

    fn next_param(&mut self) {
        self.params.count = self
            .params
            .count
            .max((self.current_param + 1).min(PARAM_LIMIT));
        if self.current_param + 1 >= PARAM_LIMIT {
            self.parameter_overflow = true;
        } else {
            self.current_param += 1;
        }
        self.current_subparam = None;
    }

    fn next_subparam(&mut self) {
        if self.current_param >= PARAM_LIMIT {
            return;
        }
        self.params.count = self.params.count.max(self.current_param + 1);
        let param = &mut self.params.values[self.current_param];
        let next = self
            .current_subparam
            // One past the limit marks an overflowed subparameter whose digits are
            // ignored; staying there keeps the parser state bounded and checkpointable.
            .map_or(0, |index| index.saturating_add(1).min(SUBPARAM_LIMIT));
        self.current_subparam = Some(next);
        param.subparam_count = param.subparam_count.max((next + 1).min(SUBPARAM_LIMIT));
    }

    fn finish_params(&mut self) {
        if self.current_param < PARAM_LIMIT
            && (self.params.count > 0 || self.current_subparam.is_some())
        {
            self.params.count = self.params.count.max(self.current_param + 1);
        }
    }
}

fn deserialize_parser_string<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BytesVisitor(PhantomData<u8>);

    impl<'de> Visitor<'de> for BytesVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_CHECKPOINT_PARSER_STRING_BYTES} parser string bytes"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|length| length > MAX_CHECKPOINT_PARSER_STRING_BYTES)
            {
                return Err(serde::de::Error::custom(
                    "parser string exceeds checkpoint bound",
                ));
            }
            let mut bytes = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_CHECKPOINT_PARSER_STRING_BYTES),
            );
            while let Some(byte) = sequence.next_element()? {
                if bytes.len() == MAX_CHECKPOINT_PARSER_STRING_BYTES {
                    return Err(serde::de::Error::custom(
                        "parser string exceeds checkpoint bound",
                    ));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }

    deserializer.deserialize_seq(BytesVisitor(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_utf8_replacement_reprocesses_ascii() {
        let mut parser = Parser::new(16, 16);
        assert_eq!(parser.feed(0xe2), (None, false));
        assert_eq!(parser.feed(b'A'), (Some(Action::Print('\u{fffd}')), true));
        assert_eq!(parser.feed(b'A'), (Some(Action::Print('A')), false));
    }

    #[test]
    fn osc_c1_terminator_is_not_taken_from_inside_utf8_text() {
        // U+4F5C is e4 bd 9c and U+271C is e2 9c 9c.
        for text in ["作業", "\u{271c}"] {
            let mut parser = Parser::new(64, 16);
            let mut sequence = b"\x1b]2;".to_vec();
            sequence.extend_from_slice(text.as_bytes());
            let mut finished = None;
            for byte in &sequence {
                assert_eq!(parser.feed(*byte).0, None);
            }
            assert_eq!(parser.state, State::OscString);
            if let (Some(action), _) = parser.feed(0x9c) {
                finished = Some(action);
            }
            let mut expected = b"2;".to_vec();
            expected.extend_from_slice(text.as_bytes());
            assert!(
                matches!(&finished, Some(Action::Osc(payload, ..)) if *payload == expected),
                "{finished:?}"
            );
            assert_eq!(parser.state, State::Ground);
        }

        // A truncated sequence does not swallow the terminator indefinitely.
        let mut parser = Parser::new(64, 16);
        for byte in b"\x1b]2;a\xe4b" {
            let _ = parser.feed(*byte);
        }
        assert!(parser.feed(0x9c).0.is_some());
        assert_eq!(parser.state, State::Ground);
    }

    #[test]
    fn osc_overflow_stays_synchronized() {
        let mut parser = Parser::new(2, 2);
        for byte in b"\x1b]2;abc\x07" {
            let _ = parser.feed(*byte);
        }
        assert_eq!(parser.state, State::Ground);
        assert_eq!(parser.feed(b'Z'), (Some(Action::Print('Z')), false));
    }

    #[test]
    fn sixel_is_streamed_without_using_the_collected_dcs_limit() {
        let mut parser = Parser::new(2, 1);
        let mut actions = Vec::new();
        for byte in b"\x1bP7;1;0q#1~\x1b\\" {
            let (action, again) = parser.feed(*byte);
            assert!(!again);
            if let Some(action) = action {
                actions.push(action);
            }
        }
        assert_eq!(actions.len(), 5);
        let Action::SixelBegin(params) = &actions[0] else {
            panic!("expected streaming Sixel begin");
        };
        assert_eq!(params.get(0).value(0, false), 7);
        assert_eq!(params.get(1).value(0, false), 1);
        assert_eq!(params.get(2).value(1, false), 0);
        assert_eq!(actions[1], Action::SixelData(b'#'));
        assert_eq!(actions[2], Action::SixelData(b'1'));
        assert_eq!(actions[3], Action::SixelData(b'~'));
        assert_eq!(actions[4], Action::SixelEnd);
    }

    #[test]
    fn kitty_apc_streams_only_recognized_payloads_and_recovers() {
        let mut parser = Parser::new(16, 16);
        let mut actions = Vec::new();
        for byte in b"\x1b_not-kitty\x1b\\Z\x1b_Gi=7,m=0;AAAA\x1b\\Q" {
            let (action, mut again) = parser.feed(*byte);
            if let Some(action) = action {
                actions.push(action);
            }
            while again {
                let (action, next) = parser.feed(*byte);
                if let Some(action) = action {
                    actions.push(action);
                }
                again = next;
            }
        }
        assert_eq!(
            actions,
            vec![
                Action::Print('Z'),
                Action::KittyBegin(b"i=7,m=0".to_vec(), false),
                Action::KittyData(b'A'),
                Action::KittyData(b'A'),
                Action::KittyData(b'A'),
                Action::KittyData(b'A'),
                Action::KittyEnd,
                Action::Print('Q'),
            ]
        );
    }

    #[test]
    fn kitty_control_only_and_cancel_are_bounded_and_synchronized() {
        let mut parser = Parser::new(16, 16);
        let mut actions = Vec::new();
        let mut input = b"\x1b_Ga=p,i=7\x1b\\\x1b_G".to_vec();
        input.extend(std::iter::repeat_n(b'x', 1025));
        input.extend_from_slice(b"\x1b\\\x1b_Ga=t;AAAA\x1aZ");
        for byte in input {
            let (action, mut again) = parser.feed(byte);
            if let Some(action) = action {
                actions.push(action);
            }
            while again {
                let (action, next) = parser.feed(byte);
                if let Some(action) = action {
                    actions.push(action);
                }
                again = next;
            }
        }
        assert_eq!(
            actions.first(),
            Some(&Action::KittyCommand(b"a=p,i=7".to_vec(), false))
        );
        assert!(
            matches!(actions.get(1), Some(Action::KittyCommand(control, true)) if control.len() == 1024)
        );
        assert_eq!(actions.last(), Some(&Action::Print('Z')));
        assert!(actions.contains(&Action::KittyAbort));
        assert_eq!(parser.state, State::Ground);
    }

    #[test]
    fn sixel_cancel_aborts_and_recovers_to_ground() {
        let mut parser = Parser::new(16, 16);
        let mut actions = Vec::new();
        for byte in b"\x1bPq~\x18Z" {
            let (action, mut again) = parser.feed(*byte);
            if let Some(action) = action {
                actions.push(action);
            }
            while again {
                let (action, next) = parser.feed(*byte);
                if let Some(action) = action {
                    actions.push(action);
                }
                again = next;
            }
        }
        assert_eq!(
            actions,
            vec![
                Action::SixelBegin(Box::default()),
                Action::SixelData(b'~'),
                Action::SixelAbort,
                Action::Print('Z'),
            ]
        );
        assert_eq!(parser.state, State::Ground);
    }

    #[test]
    fn an_osc52_string_leaves_no_large_buffer_or_limit_behind() {
        let feed = |parser: &mut Parser, bytes: &[u8]| {
            for byte in bytes {
                let _ = parser.feed(*byte);
            }
        };
        let large = vec![b'A'; 64 * 1024];
        // Ended by BEL or ST, or cancelled by CAN.
        for end in [&b"\x07"[..], b"\x1b\\", b"\x18"] {
            let mut parser = Parser::new(16, 16);
            feed(&mut parser, b"\x1b]52;c;");
            feed(&mut parser, &large);
            assert!(parser.string.capacity() > 16);
            feed(&mut parser, end);
            assert_eq!(parser.state, State::Ground, "{end:?}");
            assert_eq!(parser.string.capacity(), 0, "{end:?}");
            assert!(parser.checkpoint_is_valid(16, 16), "{end:?}");
            assert!(parser.checkpoint_copy().checkpoint_is_valid(16, 16));
        }
        let mut parser = Parser::new(16, 16);
        feed(&mut parser, b"\x1b]52;c;?\x07");
        assert!(parser.checkpoint_is_valid(16, 16));
        // A short OSC 52 string in progress is carried whole.
        feed(&mut parser, b"\x1b]52;c;");
        let copy = parser.checkpoint_copy();
        assert!(copy.checkpoint_is_valid(16, 16));
        assert_eq!(copy.string, b"52;c;");
        assert!(!copy.string_truncated);
    }
}
