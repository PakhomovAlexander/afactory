//! Bounded observation of declared fields in one streamed JSON document (ADR-0172).
//!
//! An oracle of output larger than any retained log must still judge what the output means,
//! not how it is formatted. The performance supervisor feeds a child's whole stdout, past its
//! retained cap and across every read boundary, to an [`Observer`], one byte at a time. The
//! observer checks that the bytes are exactly one complete JSON text (RFC 8259: UTF-8, every
//! token's grammar, nothing but whitespace after the value) and, for each declared
//! [`Selector`], reports the values its path reaches. A path names object members and `*`, every
//! element of an array, from the document's root, so a value only counts where the path places
//! it: the same field in an unrelated array or object is not that path's value. Member order,
//! whitespace, string escapes and fields no selector names are free.
//!
//! An integer selector's values must be plain nonnegative integers of at most 20 digits that fit
//! in 64 bits: a fraction, an exponent, a sign or any other kind of value is invalid, and its
//! values are observed in order, as a sequence of exactly consecutive numbers or with its first
//! break. A string selector counts the occurrences of each declared value after unescaping.
//! Within one object a selected member name that repeats is counted as a duplicate.
//!
//! Memory is bounded by the nesting depth and the declared selectors, never by the document:
//! nothing but a selected member name or string value up to its declared bound is kept.

/// The most selectors one observation declares.
pub const SELECTORS_MAX: usize = 8;
/// The most segments one selector's path has.
pub const SEGMENTS_MAX: usize = 8;
/// The longest member name a path segment names.
pub const MEMBER_BYTES_MAX: usize = 64;
/// The most declared values a string selector counts.
pub const EQUALS_MAX: usize = 4;
/// The longest declared string value.
pub const VALUE_BYTES_MAX: usize = 256;
/// The deepest nesting of objects and arrays observed; deeper documents are refused.
pub const DEPTH_MAX: usize = 512;
/// The most digits a 64-bit integer has.
const INTEGER_DIGITS_MAX: u32 = 20;

/// One step of a selector's path from the document's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// The object member of this name.
    Member(Vec<u8>),
    /// Every element of an array.
    Each,
}

/// What a selector's values must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Integer,
    String,
}

/// One declared path and how its values are observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub path: Vec<Segment>,
    pub kind: Kind,
    /// A string selector's declared values, whose occurrences are counted; empty for an integer
    /// selector.
    pub equals: Vec<Vec<u8>>,
}

impl Selector {
    /// A selector of a dotted `path` (`history.*.sequence`): one to eight segments, each `*` or
    /// a member name of 1 to 64 characters of `[a-z0-9_]`. A string selector declares one to four
    /// values of 1 to 256 bytes; an integer selector none.
    pub fn parse(path: &str, kind: Kind, equals: Vec<Vec<u8>>) -> Result<Self, String> {
        let segments: Vec<&str> = path.split('.').collect();
        if segments.len() > SEGMENTS_MAX {
            return Err(format!(
                "path {path:?} has more than {SEGMENTS_MAX} segments"
            ));
        }
        let mut parsed = Vec::with_capacity(segments.len());
        for segment in segments {
            if segment == "*" {
                parsed.push(Segment::Each);
            } else if !segment.is_empty()
                && segment.len() <= MEMBER_BYTES_MAX
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                parsed.push(Segment::Member(segment.as_bytes().to_vec()));
            } else {
                return Err(format!(
                    "path {path:?}: each segment is * or 1 to 64 of [a-z0-9_]"
                ));
            }
        }
        let declared = match kind {
            Kind::Integer => equals.is_empty(),
            Kind::String => {
                (1..=EQUALS_MAX).contains(&equals.len())
                    && equals
                        .iter()
                        .all(|value| (1..=VALUE_BYTES_MAX).contains(&value.len()))
            }
        };
        if !declared {
            return Err(format!(
                "path {path:?}: an integer selector declares no values and a string selector \
                 1 to {EQUALS_MAX} values of 1 to {VALUE_BYTES_MAX} bytes"
            ));
        }
        Ok(Self {
            path: parsed,
            kind,
            equals,
        })
    }
}

/// Whether the stream was exactly one complete JSON document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Document {
    /// One complete value, followed by nothing but whitespace.
    Complete,
    /// Bytes that are not JSON, at this offset.
    Malformed { offset: u64, detail: &'static str },
    /// The stream ended, or has not yet ended, before the document did.
    Incomplete { offset: u64 },
    /// Objects and arrays nested deeper than [`DEPTH_MAX`].
    TooDeep { offset: u64 },
}

impl Document {
    pub fn name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Malformed { .. } => "malformed",
            Self::Incomplete { .. } => "incomplete",
            Self::TooDeep { .. } => "too_deep",
        }
    }

    pub fn offset(self) -> Option<u64> {
        match self {
            Self::Complete => None,
            Self::Malformed { offset, .. }
            | Self::Incomplete { offset }
            | Self::TooDeep { offset } => Some(offset),
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Complete => "",
            Self::Malformed { detail, .. } => detail,
            Self::Incomplete { .. } => "the document is not complete",
            Self::TooDeep { .. } => "objects and arrays nest deeper than 512 levels",
        }
    }
}

/// Where an integer selector's sequence first broke: the occurrence's zero-based index, the
/// previous occurrence's integer and its own, each if it was a valid integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Break {
    pub index: u64,
    pub after: Option<u64>,
    pub value: Option<u64>,
}

/// What one selector's path reached.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selected {
    /// Values its parent path reached: each object or array that could hold one of its values.
    pub holders: u64,
    /// Values its path reached.
    pub count: u64,
    /// Selected member names that repeated within one object along its path.
    pub duplicates: u64,
    /// Values that are not of the selector's kind.
    pub invalid: u64,
    /// The zero-based index of the first of them.
    pub first_invalid: Option<u64>,
    /// An integer selector's first and last value, if each was a valid integer.
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// An integer selector's values that were not the previous value plus one.
    pub breaks: u64,
    pub first_break: Option<Break>,
    /// A string selector's occurrences of each declared value.
    pub matches: Vec<u64>,
}

/// The document's status and every selector's observation, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub document: Document,
    pub selectors: Vec<Selected>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Value,
    ValueOrEnd,
    KeyOrEnd,
    Key,
    Colon,
    CommaOrEnd,
    Trailing,
}

/// One open object or array: the selectors whose paths continue below it, and in an object the
/// selected member names seen so far and the selectors the current member's value reaches.
struct Frame {
    array: bool,
    descend: u16,
    seen: u16,
    child: u16,
}

#[derive(Clone, Copy)]
enum Escape {
    None,
    Backslash,
    Unicode {
        digits: u8,
        code: u32,
        high: Option<u32>,
    },
    AfterHigh(u32),
    AfterHighBackslash(u32),
}

struct Text {
    key: bool,
    capture: bool,
    limit: usize,
    buffer: Vec<u8>,
    /// The decoded string is longer than its bound or holds a lone surrogate: it matches
    /// nothing.
    unmatchable: bool,
    escape: Escape,
    /// Continuation bytes still due and the range the next one must fall in.
    due: u8,
    low: u8,
    high: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Minus,
    Zero,
    Int,
    Dot,
    Frac,
    Exp,
    ExpSign,
    ExpDigits,
}

struct Number {
    phase: Phase,
    plain: bool,
    digits: u32,
    value: u64,
    overflow: bool,
}

enum Scalar {
    None,
    Text(Box<Text>),
    Number(Number),
    Literal { word: &'static [u8], at: usize },
}

/// Observes one streamed JSON document; feed it every byte read, in order, then `finish` it.
pub struct Observer {
    selectors: Vec<Selector>,
    observed: Vec<Selected>,
    previous: Vec<Option<u64>>,
    stack: Vec<Frame>,
    expect: Expect,
    scalar: Scalar,
    /// The selectors whose path ends at the value being read.
    targets: u16,
    offset: u64,
    failed: Option<Document>,
    started: bool,
    finished: bool,
}

impl Observer {
    /// An observer of at most [`SELECTORS_MAX`] selectors; any beyond them are ignored.
    pub fn new(mut selectors: Vec<Selector>) -> Self {
        selectors.truncate(SELECTORS_MAX);
        let observed = selectors
            .iter()
            .map(|selector| Selected {
                matches: vec![0; selector.equals.len()],
                ..Selected::default()
            })
            .collect();
        let previous = vec![None; selectors.len()];
        Self {
            selectors,
            observed,
            previous,
            stack: Vec::new(),
            expect: Expect::Value,
            scalar: Scalar::None,
            targets: 0,
            offset: 0,
            failed: None,
            started: false,
            finished: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if self.failed.is_some() {
                return;
            }
            self.byte(byte);
            self.offset += 1;
        }
    }

    /// The stream ended: a number it ended in is complete now, and anything else unfinished
    /// leaves the document incomplete.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.failed.is_some() {
            return;
        }
        let number = matches!(&self.scalar, Scalar::Number(number)
            if matches!(number.phase, Phase::Zero | Phase::Int | Phase::Frac | Phase::ExpDigits));
        if number {
            self.end_number();
        } else if !matches!(self.scalar, Scalar::None) {
            self.failed = Some(Document::Incomplete {
                offset: self.offset,
            });
        }
    }

    /// What was observed so far. Until `finish`, the document is never complete.
    pub fn observation(&self) -> Observation {
        let document = match self.failed {
            Some(failed) => failed,
            None if self.finished && self.expect == Expect::Trailing => Document::Complete,
            None => Document::Incomplete {
                offset: self.offset,
            },
        };
        Observation {
            document,
            selectors: self.observed.clone(),
        }
    }

    fn fail(&mut self, detail: &'static str) {
        self.failed = Some(Document::Malformed {
            offset: self.offset,
            detail,
        });
    }

    fn byte(&mut self, byte: u8) {
        match &mut self.scalar {
            Scalar::Text(_) => return self.text_byte(byte),
            Scalar::Literal { word, at } => {
                if byte != word[*at] {
                    return self.fail("an invalid literal");
                }
                *at += 1;
                if *at == word.len() {
                    self.scalar = Scalar::None;
                    self.end_other(false);
                }
                return;
            }
            Scalar::Number(_) => {
                if self.number_byte(byte) {
                    return;
                }
                self.end_number();
            }
            Scalar::None => {}
        }
        self.structural(byte);
    }

    fn structural(&mut self, byte: u8) {
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            return;
        }
        match self.expect {
            Expect::Trailing => self.fail("content after the document"),
            Expect::Value | Expect::ValueOrEnd => {
                if byte == b']' && self.expect == Expect::ValueOrEnd {
                    self.close();
                } else {
                    self.begin_value(byte);
                }
            }
            Expect::KeyOrEnd | Expect::Key => {
                if byte == b'}' && self.expect == Expect::KeyOrEnd {
                    self.close();
                } else if byte == b'"' {
                    let capture = self.stack.last().is_some_and(|frame| frame.descend != 0);
                    self.scalar =
                        Scalar::Text(Box::new(Text::new(true, capture, MEMBER_BYTES_MAX)));
                } else {
                    self.fail("expected a member name");
                }
            }
            Expect::Colon => {
                if byte == b':' {
                    self.expect = Expect::Value;
                } else {
                    self.fail("expected ':' after a member name");
                }
            }
            Expect::CommaOrEnd => {
                let array = self.stack.last().is_some_and(|frame| frame.array);
                match byte {
                    b',' => self.expect = if array { Expect::Value } else { Expect::Key },
                    b']' if array => self.close(),
                    b'}' if !array => self.close(),
                    _ => self.fail("expected ',' or the end of the enclosing value"),
                }
            }
        }
    }

    /// The selectors whose paths have exactly `length` segments, among `mask`.
    fn length_mask(&self, mask: u16, length: usize) -> u16 {
        self.selectors
            .iter()
            .enumerate()
            .filter(|(index, selector)| mask & (1 << index) != 0 && selector.path.len() == length)
            .fold(0, |found, (index, _)| found | (1 << index))
    }

    fn longer_mask(&self, mask: u16, length: usize) -> u16 {
        self.selectors
            .iter()
            .enumerate()
            .filter(|(index, selector)| mask & (1 << index) != 0 && selector.path.len() > length)
            .fold(0, |found, (index, _)| found | (1 << index))
    }

    /// The selectors among `mask` whose segment at `depth` is `wanted` (a member name, or every
    /// element when `None`).
    fn segment_mask(&self, mask: u16, depth: usize, wanted: Option<&[u8]>) -> u16 {
        self.selectors
            .iter()
            .enumerate()
            .filter(|(index, selector)| {
                mask & (1 << index) != 0
                    && match (&selector.path[depth], wanted) {
                        (Segment::Each, None) => true,
                        (Segment::Member(name), Some(key)) => name.as_slice() == key,
                        _ => false,
                    }
            })
            .fold(0, |found, (index, _)| found | (1 << index))
    }

    fn begin_value(&mut self, byte: u8) {
        let depth = self.stack.len();
        let reach = match self.stack.last() {
            None => (1u16 << self.selectors.len()) - 1,
            Some(frame) if frame.array => self.segment_mask(frame.descend, depth - 1, None),
            Some(frame) => frame.child,
        };
        self.started = true;
        let holders = self.length_mask(reach, depth + 1);
        for (index, observed) in self.observed.iter_mut().enumerate() {
            if holders & (1 << index) != 0 {
                observed.holders += 1;
            }
        }
        self.targets = self.length_mask(reach, depth);
        let descend = self.longer_mask(reach, depth);
        match byte {
            b'{' | b'[' => {
                // A container where a selector's value belongs is not of its kind.
                self.end_other(true);
                if self.stack.len() >= DEPTH_MAX {
                    self.failed = Some(Document::TooDeep {
                        offset: self.offset,
                    });
                    return;
                }
                let array = byte == b'[';
                self.stack.push(Frame {
                    array,
                    descend,
                    seen: 0,
                    child: 0,
                });
                self.expect = if array {
                    Expect::ValueOrEnd
                } else {
                    Expect::KeyOrEnd
                };
            }
            b'"' => {
                let capture = self.selectors.iter().enumerate().any(|(index, selector)| {
                    self.targets & (1 << index) != 0 && selector.kind == Kind::String
                });
                self.scalar = Scalar::Text(Box::new(Text::new(false, capture, VALUE_BYTES_MAX)));
            }
            b'-' => {
                self.scalar = Scalar::Number(Number {
                    phase: Phase::Minus,
                    plain: false,
                    digits: 0,
                    value: 0,
                    overflow: false,
                });
            }
            b'0'..=b'9' => {
                self.scalar = Scalar::Number(Number {
                    phase: if byte == b'0' {
                        Phase::Zero
                    } else {
                        Phase::Int
                    },
                    plain: true,
                    digits: 1,
                    value: u64::from(byte - b'0'),
                    overflow: false,
                });
            }
            b't' => {
                self.scalar = Scalar::Literal {
                    word: b"true",
                    at: 1,
                }
            }
            b'f' => {
                self.scalar = Scalar::Literal {
                    word: b"false",
                    at: 1,
                }
            }
            b'n' => {
                self.scalar = Scalar::Literal {
                    word: b"null",
                    at: 1,
                }
            }
            _ => self.fail("expected a value"),
        }
    }

    fn close(&mut self) {
        self.stack.pop();
        self.after_value();
    }

    fn after_value(&mut self) {
        self.expect = if self.stack.is_empty() {
            Expect::Trailing
        } else {
            Expect::CommaOrEnd
        };
    }

    /// A value of no selector's kind reached its targets: a literal, which ends here, or a
    /// container, recorded when it opens.
    fn end_other(&mut self, container: bool) {
        self.record(None, None, false);
        if !container {
            self.after_value();
        }
    }

    /// One value of each target selector: a number (`integer` is `Some` when it is a plain
    /// nonnegative 64-bit integer), a decoded string and whether it can match, or neither.
    fn record(&mut self, integer: Option<u64>, text: Option<(&[u8], bool)>, number: bool) {
        let targets = self.targets;
        self.targets = 0;
        for (index, selector) in self.selectors.iter().enumerate() {
            if targets & (1 << index) == 0 {
                continue;
            }
            let observed = &mut self.observed[index];
            let at = observed.count;
            observed.count += 1;
            let valid = match selector.kind {
                Kind::Integer => {
                    let value = if number { integer } else { None };
                    if at == 0 {
                        observed.first = value;
                    } else {
                        let follows = matches!((self.previous[index], value),
                            (Some(previous), Some(value)) if previous.checked_add(1) == Some(value));
                        if !follows {
                            observed.breaks += 1;
                            if observed.first_break.is_none() {
                                observed.first_break = Some(Break {
                                    index: at,
                                    after: self.previous[index],
                                    value,
                                });
                            }
                        }
                    }
                    observed.last = value;
                    self.previous[index] = value;
                    value.is_some()
                }
                Kind::String => match text {
                    Some((decoded, matchable)) => {
                        if matchable {
                            for (wanted, matched) in
                                selector.equals.iter().zip(observed.matches.iter_mut())
                            {
                                if wanted.as_slice() == decoded {
                                    *matched += 1;
                                }
                            }
                        }
                        true
                    }
                    None => false,
                },
            };
            if !valid {
                observed.invalid += 1;
                observed.first_invalid.get_or_insert(at);
            }
        }
    }

    fn number_byte(&mut self, byte: u8) -> bool {
        let Scalar::Number(number) = &mut self.scalar else {
            return false;
        };
        let digit = byte.is_ascii_digit();
        let next = match (number.phase, byte) {
            (Phase::Minus, b'0') => Some(Phase::Zero),
            (Phase::Minus, _) if digit => Some(Phase::Int),
            (Phase::Minus, _) => return self.malformed_number(),
            (Phase::Zero, _) if digit => return self.malformed_number(),
            (Phase::Zero | Phase::Int, b'.') => Some(Phase::Dot),
            (Phase::Zero | Phase::Int | Phase::Frac, b'e' | b'E') => Some(Phase::Exp),
            (Phase::Int, _) if digit => {
                let value = number
                    .value
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(u64::from(byte - b'0')));
                number.digits = number.digits.saturating_add(1);
                number.overflow |= value.is_none() || number.digits > INTEGER_DIGITS_MAX;
                number.value = value.unwrap_or(0);
                Some(Phase::Int)
            }
            (Phase::Dot | Phase::Frac, _) if digit => Some(Phase::Frac),
            (Phase::Dot, _) => return self.malformed_number(),
            (Phase::Exp, b'+' | b'-') => Some(Phase::ExpSign),
            (Phase::Exp | Phase::ExpSign | Phase::ExpDigits, _) if digit => Some(Phase::ExpDigits),
            (Phase::Exp | Phase::ExpSign, _) => return self.malformed_number(),
            _ => None,
        };
        match next {
            Some(phase) => {
                if matches!(phase, Phase::Dot | Phase::Exp) {
                    number.plain = false;
                }
                number.phase = phase;
                true
            }
            None => false,
        }
    }

    fn malformed_number(&mut self) -> bool {
        self.fail("an invalid number");
        true
    }

    fn end_number(&mut self) {
        let Scalar::Number(number) = std::mem::replace(&mut self.scalar, Scalar::None) else {
            return;
        };
        let integer = (number.plain && !number.overflow).then_some(number.value);
        self.record(integer, None, true);
        self.after_value();
    }

    fn text_byte(&mut self, byte: u8) {
        let Scalar::Text(text) = &mut self.scalar else {
            return;
        };
        if text.due > 0 {
            if byte < text.low || byte > text.high {
                return self.fail("invalid UTF-8 in a string");
            }
            text.due -= 1;
            text.low = 0x80;
            text.high = 0xBF;
            text.push(byte);
            return;
        }
        let mut byte_escape = text.escape;
        loop {
            match byte_escape {
                Escape::None => break,
                Escape::Backslash => {
                    let decoded = match byte {
                        b'"' | b'\\' | b'/' => byte,
                        b'b' => 0x08,
                        b'f' => 0x0C,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'u' => {
                            text.escape = Escape::Unicode {
                                digits: 0,
                                code: 0,
                                high: None,
                            };
                            return;
                        }
                        _ => return self.fail("an invalid escape in a string"),
                    };
                    text.push(decoded);
                    text.escape = Escape::None;
                    return;
                }
                Escape::Unicode { digits, code, high } => {
                    let Some(nibble) = (byte as char).to_digit(16) else {
                        return self.fail("an invalid \\u escape in a string");
                    };
                    let code = code * 16 + nibble;
                    if digits < 3 {
                        text.escape = Escape::Unicode {
                            digits: digits + 1,
                            code,
                            high,
                        };
                        return;
                    }
                    text.code_point(code, high);
                    return;
                }
                Escape::AfterHigh(high) => {
                    if byte == b'\\' {
                        text.escape = Escape::AfterHighBackslash(high);
                        return;
                    }
                    // A high surrogate no low one follows.
                    text.unmatchable = true;
                    text.escape = Escape::None;
                    byte_escape = Escape::None;
                }
                Escape::AfterHighBackslash(high) => {
                    if byte == b'u' {
                        text.escape = Escape::Unicode {
                            digits: 0,
                            code: 0,
                            high: Some(high),
                        };
                        return;
                    }
                    text.unmatchable = true;
                    text.escape = Escape::Backslash;
                    byte_escape = Escape::Backslash;
                }
            }
        }
        match byte {
            b'"' => self.end_text(),
            b'\\' => text.escape = Escape::Backslash,
            0x00..=0x1F => self.fail("a control character in a string"),
            0x20..=0x7F => text.push(byte),
            _ => {
                let (due, low, high) = match byte {
                    0xC2..=0xDF => (1, 0x80, 0xBF),
                    0xE0 => (2, 0xA0, 0xBF),
                    0xE1..=0xEC | 0xEE..=0xEF => (2, 0x80, 0xBF),
                    0xED => (2, 0x80, 0x9F),
                    0xF0 => (3, 0x90, 0xBF),
                    0xF1..=0xF3 => (3, 0x80, 0xBF),
                    0xF4 => (3, 0x80, 0x8F),
                    _ => return self.fail("invalid UTF-8 in a string"),
                };
                text.due = due;
                text.low = low;
                text.high = high;
                text.push(byte);
            }
        }
    }

    fn end_text(&mut self) {
        let Scalar::Text(text) = std::mem::replace(&mut self.scalar, Scalar::None) else {
            return;
        };
        if text.key {
            let Some(frame) = self.stack.last() else {
                return;
            };
            let depth = self.stack.len() - 1;
            let child = if text.capture && !text.unmatchable {
                self.segment_mask(frame.descend, depth, Some(&text.buffer))
            } else {
                0
            };
            let repeated = child & frame.seen;
            for (index, observed) in self.observed.iter_mut().enumerate() {
                if repeated & (1 << index) != 0 {
                    observed.duplicates += 1;
                }
            }
            if let Some(frame) = self.stack.last_mut() {
                frame.seen |= child;
                frame.child = child;
            }
            self.expect = Expect::Colon;
            return;
        }
        self.record(None, Some((&text.buffer, !text.unmatchable)), false);
        self.after_value();
    }
}

impl Text {
    fn new(key: bool, capture: bool, limit: usize) -> Self {
        Self {
            key,
            capture,
            limit,
            buffer: Vec::new(),
            unmatchable: false,
            escape: Escape::None,
            due: 0,
            low: 0x80,
            high: 0xBF,
        }
    }

    fn push(&mut self, byte: u8) {
        if !self.capture {
            return;
        }
        if self.buffer.len() < self.limit {
            self.buffer.push(byte);
        } else {
            self.unmatchable = true;
        }
    }

    /// A decoded `\u` escape, after a pending high surrogate when there is one.
    fn code_point(&mut self, code: u32, high: Option<u32>) {
        self.escape = Escape::None;
        let code = match high {
            Some(high) if (0xDC00..=0xDFFF).contains(&code) => {
                0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00)
            }
            Some(_) => {
                // The pending high surrogate is lone; this escape stands by itself.
                self.unmatchable = true;
                code
            }
            None => code,
        };
        if (0xD800..=0xDBFF).contains(&code) {
            self.escape = Escape::AfterHigh(code);
            return;
        }
        match char::from_u32(code) {
            Some(decoded) => {
                let mut encoded = [0; 4];
                for &byte in decoded.encode_utf8(&mut encoded).as_bytes() {
                    self.push(byte);
                }
            }
            None => self.unmatchable = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selectors() -> Vec<Selector> {
        vec![
            Selector::parse("task_id", Kind::String, vec![b"perf-probe-task".to_vec()]).unwrap(),
            Selector::parse("history.*.sequence", Kind::Integer, Vec::new()).unwrap(),
            Selector::parse(
                "history.*.transition.change.kind",
                Kind::String,
                vec![b"lease_renewed".to_vec(), b"opened".to_vec()],
            )
            .unwrap(),
        ]
    }

    fn observe_split(text: &[u8], split: &[usize]) -> Observation {
        let mut observer = Observer::new(selectors());
        let mut rest = text;
        for &size in split {
            let (head, tail) = rest.split_at(size.min(rest.len()));
            observer.feed(head);
            rest = tail;
        }
        observer.feed(rest);
        observer.finish();
        observer.observation()
    }

    fn observe(text: &[u8]) -> Observation {
        observe_split(text, &[])
    }

    fn entry(sequence: &str, kind: &str) -> String {
        format!("{{\"sequence\":{sequence},\"transition\":{{\"change\":{{\"kind\":\"{kind}\"}}}}}}")
    }

    fn document(entries: &[String]) -> Vec<u8> {
        format!(
            "{{\"task_id\":\"perf-probe-task\",\"history\":[{}]}}\n",
            entries.join(",")
        )
        .into_bytes()
    }

    fn history(count: u64) -> Vec<String> {
        (0..count)
            .map(|index| {
                entry(
                    &index.to_string(),
                    if index == 0 {
                        "opened"
                    } else {
                        "lease_renewed"
                    },
                )
            })
            .collect()
    }

    fn whole(count: u64) -> Observation {
        Observation {
            document: Document::Complete,
            selectors: vec![
                Selected {
                    holders: 1,
                    count: 1,
                    matches: vec![1],
                    ..Selected::default()
                },
                Selected {
                    holders: count,
                    count,
                    first: Some(0),
                    last: Some(count - 1),
                    ..Selected::default()
                },
                Selected {
                    holders: count,
                    count,
                    matches: vec![count - 1, 1],
                    ..Selected::default()
                },
            ],
        }
    }

    #[test]
    fn a_whole_document_is_observed_across_every_chunk_boundary() {
        let text = document(&history(12));
        assert_eq!(observe(&text), whole(12));
        for split in 0..=text.len() {
            assert_eq!(
                observe_split(&text, &[split]),
                whole(12),
                "split at {split}"
            );
        }
        assert_eq!(observe_split(&text, &vec![1; text.len()]), whole(12));
    }

    #[test]
    fn whitespace_member_order_escapes_and_undeclared_fields_are_free() {
        let reordered = br#"
            { "history" : [
                { "transition": {"note": null, "change": {"kind": "opened", "at": 1}},
                  "sequence" : 0 },
                {"extra": [1, 2.5e3, {"sequence": 99}], "sequence": 1,
                 "transition": {"change": {"kind": "lease_renewed"}}}
              ],
              "other": {"history": [{"sequence": 7}]},
              "task_id" : "perf-probe-task" }
        "#;
        let observed = observe(reordered);
        assert_eq!(observed, whole(2));
    }

    #[test]
    fn values_outside_their_path_are_never_counted() {
        // The root's counterexample: every entry but the first moved into an unrelated array.
        let moved = format!(
            "{{\"task_id\":\"perf-probe-task\",\"history\":[{}],\"unrelated\":[{}]}}",
            history(1)[0],
            history(5)[1..].join(",")
        );
        let observed = observe(moved.as_bytes());
        assert_eq!(observed.document, Document::Complete);
        assert_eq!(
            (observed.selectors[1].holders, observed.selectors[1].count),
            (1, 1)
        );
        // An entry nested one level deeper, or one without its member, is not a value.
        let nested = document(&[
            entry("0", "opened"),
            format!("{{\"inner\":{}}}", entry("1", "x")),
        ]);
        let observed = observe(&nested);
        assert_eq!(
            (observed.selectors[1].holders, observed.selectors[1].count),
            (2, 1)
        );
    }

    #[test]
    fn fractions_exponents_signs_and_other_kinds_are_invalid_integers() {
        for bad in [
            "2.5",
            "2e1",
            "2E+0",
            "-2",
            "\"2\"",
            "true",
            "null",
            "[2]",
            "{}",
            "18446744073709551616",
            "000",
        ] {
            let mut entries = history(4);
            entries[2] = entry(bad, "lease_renewed");
            let observed = observe(&document(&entries));
            if bad == "000" {
                assert!(
                    matches!(observed.document, Document::Malformed { .. }),
                    "{bad}"
                );
                continue;
            }
            assert_eq!(observed.document, Document::Complete, "{bad}");
            let sequence = &observed.selectors[1];
            assert_eq!((sequence.count, sequence.invalid), (4, 1), "{bad}");
            assert_eq!(sequence.first_invalid, Some(2), "{bad}");
            assert_eq!(
                sequence.first_break,
                Some(Break {
                    index: 2,
                    after: Some(1),
                    value: None
                }),
                "{bad}"
            );
        }
        let largest = observe(&document(&[entry("18446744073709551615", "opened")]));
        assert_eq!(largest.selectors[1].first, Some(u64::MAX));
        assert_eq!(largest.selectors[1].invalid, 0);
    }

    #[test]
    fn duplicates_reorders_gaps_and_repeated_members_are_observed() {
        let numbers = |list: &[&str]| -> Observation {
            observe(&document(
                &list
                    .iter()
                    .map(|n| entry(n, "lease_renewed"))
                    .collect::<Vec<_>>(),
            ))
        };
        let duplicated = numbers(&["0", "1", "1", "2"]);
        assert_eq!(duplicated.selectors[1].breaks, 1);
        assert_eq!(
            duplicated.selectors[1].first_break,
            Some(Break {
                index: 2,
                after: Some(1),
                value: Some(1)
            })
        );
        assert_eq!(numbers(&["0", "2", "1", "3"]).selectors[1].breaks, 3);
        assert_eq!(numbers(&["0", "1", "3"]).selectors[1].breaks, 1);
        // A member repeated in one object is a duplicate, wherever along the path.
        let repeated = br#"{"task_id":"perf-probe-task","history":[{"sequence":0,"sequence":0}],
                           "task_id":"perf-probe-task"}"#;
        let observed = observe(repeated);
        assert_eq!(observed.selectors[0].duplicates, 1);
        assert_eq!(observed.selectors[1].duplicates, 1);
        assert_eq!(observed.selectors[1].count, 2);
    }

    #[test]
    fn malformed_and_incomplete_documents_are_never_complete() {
        let text = document(&history(3));
        let cases: &[&[u8]] = &[
            &text[..text.len() - 3],
            b"",
            b"   ",
            br#"{"task_id":"perf-probe-task""#,
            br#"{"task_id":"perf-probe-task",}"#,
            br#"{"history":[1,]}"#,
            br#"{"history":[1 2]}"#,
            br#"{"a":1}{"b":2}"#,
            br#"{"a":tru}"#,
            br#"{"a":"\x"}"#,
            br#"{"a":"\u12g4"}"#,
            b"{\"a\":\"\x01\"}",
            b"{\"a\":\"\xff\"}",
            b"{\"a\":\"\xc3\"}",
            b"{\"a\":\"\xed\xa0\x80\"}",
            br#"{"a":01}"#,
            br#"{"a":1.}"#,
            br#"{"a":1e}"#,
            br#"{"a":-}"#,
            br#"{'a':1}"#,
            br#"[1]]"#,
            br#"{"a":1]"#,
            b"\xef\xbb\xbf{}",
        ];
        for case in cases {
            let observed = observe(case);
            assert_ne!(
                observed.document,
                Document::Complete,
                "{}",
                String::from_utf8_lossy(case)
            );
        }
        let truncated = observe(&text[..text.len() - 3]);
        assert!(matches!(truncated.document, Document::Incomplete { .. }));
        // Unfinished, a document is never complete, however much was read.
        let mut observer = Observer::new(selectors());
        observer.feed(&text);
        assert!(matches!(
            observer.observation().document,
            Document::Incomplete { .. }
        ));
        observer.finish();
        assert_eq!(observer.observation().document, Document::Complete);
        // Too deep a nesting is refused without unbounded memory.
        let deep = [vec![b'['; DEPTH_MAX + 1], vec![b']'; DEPTH_MAX + 1]].concat();
        assert!(matches!(observe(&deep).document, Document::TooDeep { .. }));
        let fits = [vec![b'['; DEPTH_MAX], vec![b']'; DEPTH_MAX]].concat();
        assert_eq!(observe(&fits).document, Document::Complete);
    }

    #[test]
    fn strings_decode_escapes_and_surrogates_before_they_are_compared() {
        let paired = br#"{"task_id":"perf-probe-task","history":[{"sequence":0,
            "transition":{"change":{"kind":"\ud83d\ude00"}}}]}"#;
        let mut observer = Observer::new(vec![
            Selector::parse(
                "history.*.transition.change.kind",
                Kind::String,
                vec!["\u{1F600}".as_bytes().to_vec()],
            )
            .unwrap(),
        ]);
        observer.feed(paired);
        observer.finish();
        assert_eq!(observer.observation().selectors[0].matches, vec![1]);
        // A lone surrogate is valid JSON but matches nothing; an overlong value neither.
        for text in [
            &br#"{"task_id":"\ud800perf-probe-task"}"#[..],
            br#"{"task_id":"perf-probe-task\udc00"}"#,
        ] {
            let observed = observe(text);
            assert_eq!(observed.document, Document::Complete);
            assert_eq!(observed.selectors[0].matches, vec![0]);
            assert_eq!(observed.selectors[0].invalid, 0);
        }
        let long = format!("{{\"task_id\":\"{}\"}}", "p".repeat(VALUE_BYTES_MAX + 1));
        assert_eq!(observe(long.as_bytes()).selectors[0].matches, vec![0]);
        // A non-string where a string selector's value belongs is invalid.
        let observed = observe(br#"{"task_id":7}"#);
        assert_eq!(
            (observed.selectors[0].count, observed.selectors[0].invalid),
            (1, 1)
        );
    }

    #[test]
    fn selector_paths_and_values_are_declared_and_bounded() {
        assert!(Selector::parse("history.*.sequence", Kind::Integer, Vec::new()).is_ok());
        for bad in [
            "",
            "a..b",
            "A",
            "a.b-c",
            "a.b.c.d.e.f.g.h.i",
            &"x".repeat(65),
        ] {
            assert!(
                Selector::parse(bad, Kind::Integer, Vec::new()).is_err(),
                "{bad}"
            );
        }
        assert!(Selector::parse("a", Kind::Integer, vec![b"x".to_vec()]).is_err());
        assert!(Selector::parse("a", Kind::String, Vec::new()).is_err());
        assert!(Selector::parse("a", Kind::String, vec![Vec::new()]).is_err());
        assert!(Selector::parse("a", Kind::String, vec![vec![b'x'; 257]]).is_err());
        assert!(Selector::parse("a", Kind::String, vec![b"x".to_vec(); 5]).is_err());
    }
}
