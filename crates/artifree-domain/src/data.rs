//! Application data (decision 0031): the JSON value tree of one data space,
//! the path that addresses a place in it, and the three changes that can be
//! made to it.
//!
//! A data space is always a JSON object. Its values are typed here instead of
//! being passed on as untyped JSON text: a [`DataValue`] can only hold what the
//! rules below allow, so the store and the adapters never hold a value that
//! was not checked.
//!
//! - **Numbers** are JSON numbers that survive a trip through a double
//!   precision client. An integer literal must be within +-(2^53 - 1); a
//!   number with a fraction or an exponent must be finite. A number whose
//!   value is a whole number within that range is stored as an integer, so
//!   `1`, `1.0`, `1e0`, and `-0` are the same value (`1` or `0`).
//! - **Members** of an object have unique names of 1 to 256 bytes. Member
//!   order is not kept: the canonical form sorts them by UTF-8 bytes.
//! - **Text** is valid Unicode. The escapes `\ud800` to `\udfff` are accepted
//!   only as a surrogate pair.
//! - **Canonical form** is compact JSON: sorted members, no whitespace, the
//!   escapes `\" \\ \b \f \n \r \t` and `\u00xx` for other control characters,
//!   everything else as UTF-8. Parsing the canonical form gives the same
//!   value.
//!
//! No error quotes the text it rejected. The domain does no I/O.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound;
use std::str::FromStr;

/// Most segments in a path.
pub const MAX_PATH_DEPTH: usize = 16;
/// Most bytes in a path segment.
pub const MAX_SEGMENT_BYTES: usize = 64;
/// Most bytes in the name of an object member.
pub const MAX_MEMBER_NAME_BYTES: usize = 256;
/// Deepest nesting of arrays and objects in a value, and in a whole space
/// (whose root object is the first level).
pub const MAX_NESTING: usize = 32;
/// Largest value or patch a request may carry, as JSON text.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;
/// Largest data space, in canonical bytes.
pub const MAX_SPACE_BYTES: usize = 1024 * 1024;
/// Most values in a data space, counting every array, object, and scalar
/// below the root.
pub const MAX_SPACE_VALUES: usize = 10_000;
/// The largest integer a double precision number holds exactly.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Most members a page of a member query lists (decision 0040).
pub const MAX_MEMBER_LIMIT: usize = 100;
/// Members a page lists when the query gives no limit.
pub const DEFAULT_MEMBER_LIMIT: usize = 50;
/// Largest canonical value a listed member carries; a larger one is reported
/// as too large and not sent.
pub const MEMBER_VALUE_BYTES: usize = 32 * 1024;
/// Most bytes of names and values one page lists, except that its first
/// member is always listed.
pub const MEMBER_PAGE_BYTES: usize = 256 * 1024;
/// Largest canonical value a subscription sends whole.
pub const WATCHED_VALUE_BYTES: usize = 128 * 1024;

/// Which limit a request would exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLimit {
    /// The value or patch is larger than [`MAX_VALUE_BYTES`].
    ValueBytes,
    /// The space would be larger than [`MAX_SPACE_BYTES`].
    SpaceBytes,
    /// The space would hold more than [`MAX_SPACE_VALUES`] values.
    SpaceValues,
    /// Arrays and objects would be nested deeper than [`MAX_NESTING`].
    Nesting,
}

impl fmt::Display for DataLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ValueBytes => "the value is larger than 262144 bytes",
            Self::SpaceBytes => "the data space would be larger than 1048576 bytes",
            Self::SpaceValues => "the data space would hold more than 10000 values",
            Self::Nesting => "arrays and objects would be nested deeper than 32 levels",
        })
    }
}

/// Why a path, a value, or a change was refused. Messages name a rule and a
/// position, never the rejected text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataError {
    /// The path breaks the grammar.
    InvalidPath(&'static str),
    /// The text is not an acceptable JSON value.
    InvalidJson {
        /// The byte offset of the first problem.
        offset: usize,
        /// The rule it breaks.
        rule: &'static str,
    },
    /// A limit would be exceeded.
    TooLarge(DataLimit),
    /// A change has to make room on a path that crosses a value that is not an
    /// object (a string, number, boolean, null, or array).
    PathConflict,
    /// The root of a space is an object: a change cannot make it anything
    /// else.
    RootNotObject,
    /// A member query breaks a rule of decision 0040.
    InvalidQuery(&'static str),
}

impl fmt::Display for DataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(rule) => write!(formatter, "invalid data path: {rule}"),
            Self::InvalidJson { offset, rule } => {
                write!(formatter, "invalid JSON value at byte {offset}: {rule}")
            }
            Self::TooLarge(limit) => write!(formatter, "data too large: {limit}"),
            Self::PathConflict => formatter.write_str(
                "the path crosses a value that is not an object, which a write cannot make room in",
            ),
            Self::RootNotObject => formatter.write_str("the root of a data space is an object"),
            Self::InvalidQuery(rule) => write!(formatter, "invalid member query: {rule}"),
        }
    }
}

impl std::error::Error for DataError {}

/// One segment of a [`DataPath`]: 1 to 64 ASCII letters, digits, `_`, or `-`.
fn valid_segment(segment: &str) -> bool {
    (1..=MAX_SEGMENT_BYTES).contains(&segment.len())
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// A place in a data space: zero to 16 segments, written separated by `/`.
/// The empty path is the root.
///
/// Each segment is 1 to 64 ASCII letters, digits, `_`, or `-`. Nothing else is
/// accepted: no leading, trailing, or doubled `/`, no `.` or `..`, no
/// whitespace, no percent escapes. Segments are case sensitive and are the
/// names of object members, so a path never reaches into an array.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DataPath(Vec<String>);

impl DataPath {
    /// The root of the space.
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// Whether this is the root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// The segments above the last, and the last; `None` for the root.
    fn split_last(&self) -> Option<(&[String], &String)> {
        self.0.split_last().map(|(last, above)| (above, last))
    }
}

impl FromStr for DataPath {
    type Err = DataError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Ok(Self::root());
        }
        let segments: Vec<&str> = value.split('/').collect();
        if segments.len() > MAX_PATH_DEPTH {
            return Err(DataError::InvalidPath("at most 16 segments"));
        }
        if !segments.iter().all(|segment| valid_segment(segment)) {
            return Err(DataError::InvalidPath(
                "segments are 1-64 ASCII letters, digits, '_' or '-', separated by single '/'",
            ));
        }
        Ok(Self(segments.into_iter().map(str::to_owned).collect()))
    }
}

impl fmt::Display for DataPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join("/"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Repr {
    Int(i64),
    Float(f64),
}

/// A JSON number as stored: a finite number whose magnitude is at most
/// 2^53 - 1, in whatever spelling. A whole number is stored as an integer, any
/// other as a double precision number (a fraction too small for a double reads
/// as 0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataNumber(Repr);

impl DataNumber {
    /// An integer within +-(2^53 - 1), or `None`. A range check, not `abs`,
    /// so that `i64::MIN` is refused instead of overflowing.
    #[must_use]
    pub fn from_i64(value: i64) -> Option<Self> {
        (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER)
            .contains(&value)
            .then_some(Self(Repr::Int(value)))
    }

    /// A finite number of magnitude at most 2^53 - 1, or `None`. Every double
    /// beyond that is a whole number that an integer spelling of the same
    /// value would be refused for, so `1e16` and `9007199254740993.0` are
    /// refused as `10000000000000000` is. A whole number within range becomes
    /// an integer, so `1.0` and `1` are equal.
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        // Exact: 2^53 - 1 is a double, and so is every bound compared here.
        const LIMIT: f64 = 9_007_199_254_740_991.0;
        if !value.is_finite() || value.abs() > LIMIT {
            return None;
        }
        // The display of a whole double is its digits with no exponent, so a
        // whole number in range reads back as an integer without a cast, and
        // `-0` reads as 0.
        if let Ok(whole) = value.to_string().parse::<i64>() {
            return Self::from_i64(whole);
        }
        Some(Self(Repr::Float(value)))
    }

    /// The value, if it is an integer.
    #[must_use]
    pub fn as_i64(self) -> Option<i64> {
        match self.0 {
            Repr::Int(value) => Some(value),
            Repr::Float(_) => None,
        }
    }

    /// The value as a double precision number.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        match self.0 {
            // Exact: an integer here is within 2^53.
            #[expect(
                clippy::cast_precision_loss,
                reason = "integers are limited to 2^53 - 1, which a double holds exactly"
            )]
            Repr::Int(value) => value as f64,
            Repr::Float(value) => value,
        }
    }
}

impl fmt::Display for DataNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Repr::Int(value) => write!(formatter, "{value}"),
            // The shortest text that reads back as the same double; always
            // valid JSON for a finite number.
            Repr::Float(value) => write!(formatter, "{value:?}"),
        }
    }
}

/// A JSON value that satisfies the rules of this module.
#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(DataNumber),
    /// Text.
    String(String),
    /// A list, replaced as a whole by a patch.
    Array(Vec<DataValue>),
    /// Members by name, in canonical order.
    Object(BTreeMap<String, DataValue>),
}

impl DataValue {
    /// An object with no members.
    #[must_use]
    pub fn empty_object() -> Self {
        Self::Object(BTreeMap::new())
    }

    /// Parses `text` as one JSON value.
    ///
    /// # Errors
    ///
    /// [`DataError::TooLarge`] for text over [`MAX_VALUE_BYTES`] or nesting
    /// deeper than [`MAX_NESTING`]; [`DataError::InvalidJson`] for anything
    /// that is not exactly one JSON value under the rules of this module:
    /// surrounding content, a duplicate member name, an empty or overlong
    /// member name, a number of magnitude beyond 2^53 - 1 in any spelling or one
    /// that is not finite,
    /// or a lone surrogate escape.
    pub fn parse(text: &str) -> Result<Self, DataError> {
        if text.len() > MAX_VALUE_BYTES {
            return Err(DataError::TooLarge(DataLimit::ValueBytes));
        }
        Self::parse_unbounded(text)
    }

    /// As [`Self::parse`] without the size limit of a request, for reading
    /// back a stored space, which may be larger than one request.
    ///
    /// # Errors
    ///
    /// As [`Self::parse`], except for the size.
    pub fn parse_unbounded(text: &str) -> Result<Self, DataError> {
        let mut parser = Parser {
            text,
            bytes: text.as_bytes(),
            position: 0,
        };
        parser.skip_whitespace();
        let value = parser.value(0)?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err(parser.invalid("content after the value"));
        }
        Ok(value)
    }

    /// The canonical JSON text of the value.
    #[must_use]
    pub fn to_canonical(&self) -> String {
        self.to_string()
    }

    fn write(&self, out: &mut impl fmt::Write) -> fmt::Result {
        match self {
            Self::Null => out.write_str("null"),
            Self::Bool(true) => out.write_str("true"),
            Self::Bool(false) => out.write_str("false"),
            Self::Number(number) => write!(out, "{number}"),
            Self::String(text) => write_string(out, text),
            Self::Array(items) => {
                out.write_char('[')?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.write_char(',')?;
                    }
                    item.write(out)?;
                }
                out.write_char(']')
            }
            Self::Object(members) => {
                out.write_char('{')?;
                for (index, (name, member)) in members.iter().enumerate() {
                    if index > 0 {
                        out.write_char(',')?;
                    }
                    write_string(out, name)?;
                    out.write_char(':')?;
                    member.write(out)?;
                }
                out.write_char('}')
            }
        }
    }

    /// How many values this one holds, counting itself, and how deeply arrays
    /// and objects nest in it (a scalar nests 0 levels).
    fn measure(&self) -> (usize, usize) {
        match self {
            Self::Array(items) => items.iter().fold((1, 1), |(count, depth), item| {
                let (inner, nested) = item.measure();
                (count + inner, depth.max(nested + 1))
            }),
            Self::Object(members) => members.values().fold((1, 1), |(count, depth), member| {
                let (inner, nested) = member.measure();
                (count + inner, depth.max(nested + 1))
            }),
            _ => (1, 0),
        }
    }
}

impl fmt::Display for DataValue {
    /// The canonical JSON text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(formatter)
    }
}

fn write_string(out: &mut impl fmt::Write, text: &str) -> fmt::Result {
    out.write_char('"')?;
    for character in text.chars() {
        match character {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\u{8}' => out.write_str("\\b")?,
            '\u{c}' => out.write_str("\\f")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            control if u32::from(control) < 0x20 => write!(out, "\\u{:04x}", u32::from(control))?,
            other => out.write_char(other)?,
        }
    }
    out.write_char('"')
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn invalid(&self, rule: &'static str) -> DataError {
        DataError::InvalidJson {
            offset: self.position,
            rule,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, byte: u8, rule: &'static str) -> Result<(), DataError> {
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.invalid(rule))
        }
    }

    fn literal(&mut self, word: &str, value: DataValue) -> Result<DataValue, DataError> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Ok(value)
        } else {
            Err(self.invalid("expected a JSON value"))
        }
    }

    /// `nesting` is the number of arrays and objects that enclose this value.
    fn value(&mut self, nesting: usize) -> Result<DataValue, DataError> {
        match self.peek() {
            Some(b'n') => self.literal("null", DataValue::Null),
            Some(b't') => self.literal("true", DataValue::Bool(true)),
            Some(b'f') => self.literal("false", DataValue::Bool(false)),
            Some(b'"') => self.string().map(DataValue::String),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => self.array(nesting),
            Some(b'{') => self.object(nesting),
            _ => Err(self.invalid("expected a JSON value")),
        }
    }

    fn array(&mut self, nesting: usize) -> Result<DataValue, DataError> {
        if nesting + 1 > MAX_NESTING {
            return Err(DataError::TooLarge(DataLimit::Nesting));
        }
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(DataValue::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(nesting + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(DataValue::Array(items));
                }
                _ => return Err(self.invalid("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, nesting: usize) -> Result<DataValue, DataError> {
        if nesting + 1 > MAX_NESTING {
            return Err(DataError::TooLarge(DataLimit::Nesting));
        }
        self.position += 1;
        let mut members = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(DataValue::Object(members));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.invalid("expected a member name"));
            }
            let name_at = self.position;
            let name = self.string()?;
            if !(1..=MAX_MEMBER_NAME_BYTES).contains(&name.len()) {
                self.position = name_at;
                return Err(self.invalid("member names are 1-256 bytes"));
            }
            self.skip_whitespace();
            self.expect(b':', "expected ':' after a member name")?;
            self.skip_whitespace();
            let value = self.value(nesting + 1)?;
            if members.insert(name, value).is_some() {
                self.position = name_at;
                return Err(self.invalid("duplicate member name"));
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(DataValue::Object(members));
                }
                _ => return Err(self.invalid("expected ',' or '}'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, DataError> {
        self.position += 1;
        let mut out = String::new();
        loop {
            let run = self.position;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            // The run ends at an ASCII byte or the end of the text, so it ends
            // on a character boundary; it also starts on one.
            out.push_str(&self.text[run..self.position]);
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.position += 1;
                    out.push(self.escape()?);
                }
                Some(_) => return Err(self.invalid("a raw control character in a string")),
                None => return Err(self.invalid("an unterminated string")),
            }
        }
    }

    fn escape(&mut self) -> Result<char, DataError> {
        let Some(byte) = self.peek() else {
            return Err(self.invalid("an unterminated escape"));
        };
        self.position += 1;
        Ok(match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode(),
            _ => return Err(self.invalid("an unknown escape")),
        })
    }

    fn hex4(&mut self) -> Result<u32, DataError> {
        let digits = self
            .bytes
            .get(self.position..self.position + 4)
            .ok_or_else(|| self.invalid("a \\u escape needs four hexadecimal digits"))?;
        let mut value = 0_u32;
        for digit in digits {
            let nibble = char::from(*digit)
                .to_digit(16)
                .ok_or_else(|| self.invalid("a \\u escape needs four hexadecimal digits"))?;
            value = value * 16 + nibble;
        }
        self.position += 4;
        Ok(value)
    }

    fn unicode(&mut self) -> Result<char, DataError> {
        let first = self.hex4()?;
        let code = match first {
            0xD800..=0xDBFF => {
                if !self.bytes[self.position..].starts_with(b"\\u") {
                    return Err(self.invalid("a high surrogate escape needs a low one"));
                }
                self.position += 2;
                let second = self.hex4()?;
                if !(0xDC00..=0xDFFF).contains(&second) {
                    return Err(self.invalid("a high surrogate escape needs a low one"));
                }
                0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(self.invalid("a lone low surrogate escape")),
            other => other,
        };
        char::from_u32(code).ok_or_else(|| self.invalid("not a Unicode scalar value"))
    }

    fn digits(&mut self) -> usize {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        self.position - start
    }

    fn number(&mut self) -> Result<DataValue, DataError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.invalid("a number needs digits")),
        }
        let mut whole = true;
        if self.peek() == Some(b'.') {
            whole = false;
            self.position += 1;
            if self.digits() == 0 {
                return Err(self.invalid("a fraction needs digits"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            whole = false;
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if self.digits() == 0 {
                return Err(self.invalid("an exponent needs digits"));
            }
        }
        let token = &self.text[start..self.position];
        let out_of_range = DataError::InvalidJson {
            offset: start,
            rule: "a number outside what a double precision client holds exactly (magnitude beyond 2^53 - 1 in any spelling, or not finite)",
        };
        let number = if whole {
            token
                .parse::<i64>()
                .ok()
                .and_then(DataNumber::from_i64)
                .ok_or(out_of_range)?
        } else {
            token
                .parse::<f64>()
                .ok()
                .and_then(DataNumber::from_f64)
                .ok_or(out_of_range)?
        };
        Ok(DataValue::Number(number))
    }
}

/// A data space: a JSON object, within the limits of the module.
#[derive(Debug, Clone, PartialEq)]
pub struct DataSpace(DataValue);

impl Default for DataSpace {
    fn default() -> Self {
        Self::empty()
    }
}

impl DataSpace {
    /// The space with no members: the state of a space nobody wrote.
    #[must_use]
    pub fn empty() -> Self {
        Self(DataValue::empty_object())
    }

    /// A space holding `root`.
    ///
    /// # Errors
    ///
    /// [`DataError::RootNotObject`] unless `root` is an object, and
    /// [`DataError::TooLarge`] when it exceeds a limit of a space.
    pub fn new(root: DataValue) -> Result<Self, DataError> {
        if !matches!(root, DataValue::Object(_)) {
            return Err(DataError::RootNotObject);
        }
        let space = Self(root);
        space.check_limits()?;
        Ok(space)
    }

    /// Reads back a space from its canonical text.
    ///
    /// # Errors
    ///
    /// As [`DataValue::parse_unbounded`] and [`Self::new`].
    pub fn from_canonical(text: &str) -> Result<Self, DataError> {
        Self::new(DataValue::parse_unbounded(text)?)
    }

    /// The root object.
    #[must_use]
    pub fn root(&self) -> &DataValue {
        &self.0
    }

    /// The canonical text of the space.
    #[must_use]
    pub fn to_canonical(&self) -> String {
        self.0.to_canonical()
    }

    /// How many values the space holds below its root.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.0.measure().0 - 1
    }

    /// Whether the space has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(&self.0, DataValue::Object(members) if members.is_empty())
    }

    fn check_limits(&self) -> Result<(), DataError> {
        let (count, nesting) = self.0.measure();
        if nesting > MAX_NESTING {
            return Err(DataError::TooLarge(DataLimit::Nesting));
        }
        if count - 1 > MAX_SPACE_VALUES {
            return Err(DataError::TooLarge(DataLimit::SpaceValues));
        }
        if self.0.to_canonical().len() > MAX_SPACE_BYTES {
            return Err(DataError::TooLarge(DataLimit::SpaceBytes));
        }
        Ok(())
    }

    /// The value at `path`, or `None` where nothing is. A path through a value
    /// that is not an object has nothing at its end.
    #[must_use]
    pub fn get(&self, path: &DataPath) -> Option<&DataValue> {
        path.segments()
            .iter()
            .try_fold(&self.0, |value, segment| match value {
                DataValue::Object(members) => members.get(segment),
                _ => None,
            })
    }

    /// The space after `change`, and whether it differs from this one.
    ///
    /// # Errors
    ///
    /// [`DataError::PathConflict`] when a [`DataChange::Set`] or a
    /// [`DataChange::Merge`] with a patch other than `null` has to make room
    /// below a value that is not an object, [`DataError::RootNotObject`] when
    /// the root would become anything but an object, and
    /// [`DataError::TooLarge`] when the result exceeds a limit of a space. A
    /// refused change leaves this space as it was.
    pub fn apply(&self, change: &DataChange) -> Result<Applied, DataError> {
        let mut root = self.0.clone();
        match change {
            DataChange::Set { path, value } => match path.split_last() {
                None => {
                    if !matches!(value, DataValue::Object(_)) {
                        return Err(DataError::RootNotObject);
                    }
                    root = value.clone();
                }
                Some((above, last)) => {
                    let parent =
                        parent_of(&mut root, above, true)?.ok_or(DataError::PathConflict)?;
                    parent.insert(last.clone(), value.clone());
                }
            },
            DataChange::Merge { path, patch } => match (path.split_last(), patch) {
                (None, DataValue::Object(_)) => root = merge(Some(root), patch),
                (None, _) => return Err(DataError::RootNotObject),
                // A `null` patch removes whatever is at the path, as a member
                // set to `null` in a merge patch does.
                (Some((above, last)), DataValue::Null) => {
                    if let Some(parent) = parent_of(&mut root, above, false)? {
                        parent.remove(last);
                    }
                }
                (Some((above, last)), _) => {
                    let parent =
                        parent_of(&mut root, above, true)?.ok_or(DataError::PathConflict)?;
                    let merged = merge(parent.remove(last), patch);
                    parent.insert(last.clone(), merged);
                }
            },
            DataChange::Remove { path } => match path.split_last() {
                None => root = DataValue::empty_object(),
                Some((above, last)) => {
                    if let Some(parent) = parent_of(&mut root, above, false)? {
                        parent.remove(last);
                    }
                }
            },
        }
        let changed = root != self.0;
        let space = Self(root);
        if changed {
            space.check_limits()?;
        }
        Ok(Applied { space, changed })
    }
}

/// The members of the object that `above` names, found below `root`. With
/// `create`, missing objects on the way are made; a value that is in the way
/// is [`DataError::PathConflict`]. Without it, a missing or blocked path has no
/// parent and the result is `None`.
fn parent_of<'a>(
    root: &'a mut DataValue,
    above: &[String],
    create: bool,
) -> Result<Option<&'a mut BTreeMap<String, DataValue>>, DataError> {
    let DataValue::Object(members) = root else {
        return Err(DataError::RootNotObject);
    };
    let mut members = members;
    for segment in above {
        if !members.contains_key(segment) {
            if !create {
                return Ok(None);
            }
            members.insert(segment.clone(), DataValue::empty_object());
        }
        match members.get_mut(segment) {
            Some(DataValue::Object(next)) => members = next,
            _ if create => return Err(DataError::PathConflict),
            _ => return Ok(None),
        }
    }
    Ok(Some(members))
}

/// A merge patch (RFC 7396): an object patch merges into the target, member by
/// member, where a `null` member removes the target's member; any other patch
/// replaces the target. A target that is not an object counts as an empty one
/// when an object patch meets it, so the `null` members of that patch remove
/// nothing and do not appear.
fn merge(target: Option<DataValue>, patch: &DataValue) -> DataValue {
    let DataValue::Object(changes) = patch else {
        return patch.clone();
    };
    let mut members = match target {
        Some(DataValue::Object(members)) => members,
        _ => BTreeMap::new(),
    };
    for (name, change) in changes {
        if *change == DataValue::Null {
            members.remove(name);
        } else {
            let merged = merge(members.remove(name), change);
            members.insert(name.clone(), merged);
        }
    }
    DataValue::Object(members)
}

/// What a member query lists for each member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberSelect {
    /// Only the names.
    Names,
    /// The names and the values that fit [`MEMBER_VALUE_BYTES`].
    Values,
}

/// A validated query for the members of the object at a path (decision 0040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberQuery {
    path: DataPath,
    select: MemberSelect,
    limit: usize,
    after: Option<String>,
}

impl MemberQuery {
    /// A query for up to `limit` members of the object at `path`, in the order
    /// of their names as UTF-8 bytes, after the member named `after`.
    ///
    /// # Errors
    ///
    /// [`DataError::InvalidQuery`] for a limit outside `1..=`[`MAX_MEMBER_LIMIT`]
    /// or an `after` that is not 1 to [`MAX_MEMBER_NAME_BYTES`] bytes.
    pub fn new(
        path: DataPath,
        select: MemberSelect,
        limit: usize,
        after: Option<String>,
    ) -> Result<Self, DataError> {
        if !(1..=MAX_MEMBER_LIMIT).contains(&limit) {
            return Err(DataError::InvalidQuery(
                "limit is a whole number from 1 to 100",
            ));
        }
        if after
            .as_ref()
            .is_some_and(|name| name.is_empty() || name.len() > MAX_MEMBER_NAME_BYTES)
        {
            return Err(DataError::InvalidQuery("after is 1 to 256 bytes"));
        }
        Ok(Self {
            path,
            select,
            limit,
            after,
        })
    }

    /// The path of the object.
    #[must_use]
    pub fn path(&self) -> &DataPath {
        &self.path
    }

    /// What is listed for each member.
    #[must_use]
    pub fn select(&self) -> MemberSelect {
        self.select
    }

    /// The most members the page lists.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The name after which the page starts, if any.
    #[must_use]
    pub fn after(&self) -> Option<&str> {
        self.after.as_deref()
    }
}

/// What a page carries for one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberValue {
    /// The query selected names only.
    Omitted,
    /// The canonical JSON text of the value, at most [`MEMBER_VALUE_BYTES`].
    Value(String),
    /// The value is larger than [`MEMBER_VALUE_BYTES`] and is not carried.
    TooLarge,
}

/// One member of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member's name.
    pub name: String,
    /// What the query selected of its value.
    pub value: MemberValue,
}

/// A page of members (decision 0040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberPage {
    /// Whether anything is at the path.
    pub exists: bool,
    /// Whether what is there is an object.
    pub collection: bool,
    /// The members, in the order of their names as UTF-8 bytes.
    pub members: Vec<Member>,
    /// The last name listed, when members follow it.
    pub next_after: Option<String>,
}

impl DataSpace {
    /// One page of the members of the object at the query's path. A path with
    /// nothing at it, or a value that is not an object, lists nothing. The
    /// page ends at the limit, or before the member that would take the bytes
    /// of names and values over [`MEMBER_PAGE_BYTES`]; its first member is
    /// always listed, so every page makes progress.
    #[must_use]
    pub fn members(&self, query: &MemberQuery) -> MemberPage {
        let Some(found) = self.get(&query.path) else {
            return MemberPage {
                exists: false,
                collection: false,
                members: Vec::new(),
                next_after: None,
            };
        };
        let DataValue::Object(object) = found else {
            return MemberPage {
                exists: true,
                collection: false,
                members: Vec::new(),
                next_after: None,
            };
        };
        // A `BTreeMap` of `String` iterates in the order of the UTF-8 bytes.
        let start = query
            .after
            .as_deref()
            .map_or(Bound::Unbounded, Bound::Excluded);
        let mut remaining = object.range::<str, _>((start, Bound::Unbounded)).peekable();
        let mut members: Vec<Member> = Vec::new();
        let mut bytes = 0_usize;
        while members.len() < query.limit {
            let Some((name, value)) = remaining.peek() else {
                break;
            };
            let (carried, size) = match query.select {
                MemberSelect::Names => (MemberValue::Omitted, name.len()),
                MemberSelect::Values => {
                    let text = value.to_canonical();
                    if text.len() > MEMBER_VALUE_BYTES {
                        (MemberValue::TooLarge, name.len())
                    } else {
                        let size = name.len() + text.len();
                        (MemberValue::Value(text), size)
                    }
                }
            };
            if !members.is_empty() && bytes + size > MEMBER_PAGE_BYTES {
                break;
            }
            bytes += size;
            members.push(Member {
                name: (*name).clone(),
                value: carried,
            });
            remaining.next();
        }
        let next_after = remaining
            .peek()
            .and_then(|_| members.last().map(|member| member.name.clone()));
        MemberPage {
            exists: true,
            collection: true,
            members,
            next_after,
        }
    }
}

/// A change to a data space.
#[derive(Debug, Clone, PartialEq)]
pub enum DataChange {
    /// Replaces what is at the path with the value, making the objects above
    /// it that are missing. The value may be `null`: a member then holds
    /// `null`. At the root the value must be an object and replaces the
    /// space.
    Set {
        /// Where.
        path: DataPath,
        /// What.
        value: DataValue,
    },
    /// Applies a merge patch at the path, making the objects above it that
    /// are missing. A `null` patch removes what is at the path; a patch that
    /// is not an object replaces it. At the root the patch must be an object.
    Merge {
        /// Where.
        path: DataPath,
        /// The patch.
        patch: DataValue,
    },
    /// Removes what is at the path. At the root it empties the space. Removing
    /// what is not there changes nothing.
    Remove {
        /// Where.
        path: DataPath,
    },
}

/// The result of [`DataSpace::apply`].
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The space after the change.
    pub space: DataSpace,
    /// Whether it differs from the space before.
    pub changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn value(text: &str) -> DataValue {
        DataValue::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn path(text: &str) -> DataPath {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn space(text: &str) -> DataSpace {
        DataSpace::new(value(text)).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn set(from: &str, at: &str, to: &str) -> Result<String, DataError> {
        let change = DataChange::Set {
            path: path(at),
            value: value(to),
        };
        space(from).apply(&change).map(|a| a.space.to_canonical())
    }

    fn patch(from: &str, at: &str, with: &str) -> Result<String, DataError> {
        let change = DataChange::Merge {
            path: path(at),
            patch: value(with),
        };
        space(from).apply(&change).map(|a| a.space.to_canonical())
    }

    fn remove(from: &str, at: &str) -> Result<String, DataError> {
        let change = DataChange::Remove { path: path(at) };
        space(from).apply(&change).map(|a| a.space.to_canonical())
    }

    #[test]
    fn paths_accept_only_the_documented_grammar() {
        assert!(path("").is_root());
        assert_eq!(path("a/b_c/D-1").segments(), ["a", "b_c", "D-1"]);
        assert_eq!(path("a/b").to_string(), "a/b");
        let longest = "a".repeat(MAX_SEGMENT_BYTES);
        assert!(longest.parse::<DataPath>().is_ok());
        let deepest = vec!["a"; MAX_PATH_DEPTH].join("/");
        assert!(deepest.parse::<DataPath>().is_ok());
        for bad in [
            "/",
            "/a",
            "a/",
            "a//b",
            ".",
            "..",
            "a/./b",
            "a/../b",
            "a b",
            "a%2Fb",
            "a\\b",
            "caf\u{e9}",
            "a\u{0}",
            "a?b",
            "a#b",
            "a.b",
            "a\n",
            &"a".repeat(MAX_SEGMENT_BYTES + 1),
            &vec!["a"; MAX_PATH_DEPTH + 1].join("/"),
        ] {
            assert!(bad.parse::<DataPath>().is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_path_error_never_quotes_the_path() {
        let secret = "secret value!";
        let error = secret.parse::<DataPath>().err().map(|e| e.to_string());
        assert!(!error.unwrap_or_default().contains("secret"));
    }

    #[test]
    fn values_round_trip_through_the_canonical_form() {
        for (text, canonical) in [
            ("null", "null"),
            (" true ", "true"),
            ("[1, 2.5 ,\"x\"]", "[1,2.5,\"x\"]"),
            (
                "{\"b\":1,\"a\":{\"d\":[],\"c\":{}}}",
                "{\"a\":{\"c\":{},\"d\":[]},\"b\":1}",
            ),
            ("1.0", "1"),
            ("-0", "0"),
            ("-0.0", "0"),
            ("1e2", "100"),
            ("0.5e1", "5"),
            ("1.5", "1.5"),
            ("-1.25e-7", "-1.25e-7"),
            ("9007199254740991", "9007199254740991"),
            ("-9007199254740991", "-9007199254740991"),
            ("0.1e-400", "0"),
            ("-1e-999", "0"),
            ("9007199254740991.0", "9007199254740991"),
            ("\"a\\/b\\u00e9\\n\"", "\"a/b\u{e9}\\n\""),
            ("\"\\u0001\\u001f\"", "\"\\u0001\\u001f\""),
            ("\"\\ud83d\\ude00\"", "\"\u{1f600}\""),
        ] {
            let parsed = value(text);
            assert_eq!(parsed.to_canonical(), canonical, "{text}");
            assert_eq!(value(canonical), parsed, "{canonical} reads back");
        }
    }

    #[test]
    fn whole_numbers_are_integers_and_others_stay_doubles() {
        assert_eq!(value("2.0"), value("2"));
        assert_ne!(value("2.5"), value("2"));
        let fraction = value("2.5");
        assert!(matches!(fraction, DataValue::Number(n) if n.as_i64().is_none()));
        let small = value("42");
        assert!(matches!(small, DataValue::Number(n) if n.as_i64() == Some(42)));
        assert!(matches!(small, DataValue::Number(n) if (n.as_f64() - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn numbers_that_do_not_survive_a_double_are_refused() {
        for bad in [
            "9007199254740992",
            "-9007199254740992",
            "18446744073709551616",
            "99999999999999999999999999",
            "-9223372036854775808",
            "9223372036854775807",
            "-9223372036854775809",
            "18446744073709551615",
            // The same numbers in a float spelling are refused too.
            "9007199254740993.0",
            "9.007199254740993e15",
            "9007199254740992.0",
            "-9007199254740993.0",
            "9007199254740991.5",
            "1e16",
            "1e20",
            "1E300",
            "-1.5e300",
            "1e999",
            "-1e999",
            "[1e400]",
        ] {
            assert!(
                matches!(DataValue::parse(bad), Err(DataError::InvalidJson { .. })),
                "accepted {bad}"
            );
        }
    }

    /// `abs()` overflows for `i64::MIN`; the bound is a range check, so the
    /// extremes of every integer type are refused and never panic.
    #[test]
    fn the_extremes_of_integer_types_are_refused_without_overflow() {
        assert_eq!(DataNumber::from_i64(i64::MIN), None);
        assert_eq!(DataNumber::from_i64(i64::MAX), None);
        assert_eq!(DataNumber::from_i64(MAX_SAFE_INTEGER + 1), None);
        assert_eq!(DataNumber::from_i64(-MAX_SAFE_INTEGER - 1), None);
        assert!(DataNumber::from_i64(MAX_SAFE_INTEGER).is_some());
        assert!(DataNumber::from_i64(-MAX_SAFE_INTEGER).is_some());
        for extreme in [
            f64::MAX,
            f64::MIN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            assert_eq!(DataNumber::from_f64(extreme), None, "{extreme}");
        }
        assert_eq!(DataNumber::from_f64(9_007_199_254_740_992.0), None);
        assert!(DataNumber::from_f64(-9_007_199_254_740_991.0).is_some());
    }

    #[test]
    fn malformed_json_is_refused_without_quoting_it() {
        for bad in [
            "",
            " ",
            "nul",
            "True",
            "[1,]",
            "[,1]",
            "{\"a\":1,}",
            "{a:1}",
            "{'a':1}",
            "{\"a\"}",
            "{\"a\":1 \"b\":2}",
            "01",
            "+1",
            ".5",
            "1.",
            "1e",
            "1e+",
            "0x10",
            "NaN",
            "Infinity",
            "-",
            "\"unterminated",
            "\"raw\ttab\"",
            "\"\\x41\"",
            "\"\\u12\"",
            "\"\\ud800\"",
            "\"\\ud800\\u0041\"",
            "\"\\udc00\"",
            "1 2",
            "[] x",
            "\u{feff}1",
            "{\"\":1}",
        ] {
            assert!(DataValue::parse(bad).is_err(), "accepted {bad:?}");
        }
        let error = DataValue::parse("{\"secret-token\": nope}")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            !error.contains("secret") && !error.contains("nope"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_member_names_are_refused_at_every_depth() {
        for bad in [
            "{\"a\":1,\"a\":2}",
            "{\"a\":{\"b\":1,\"b\":1}}",
            "[{\"a\":1,\"a\":1}]",
            "{\"a\":1,\"\\u0061\":2}",
        ] {
            assert!(
                matches!(
                    DataValue::parse(bad),
                    Err(DataError::InvalidJson {
                        rule: "duplicate member name",
                        ..
                    })
                ),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn member_names_have_a_length_bound() {
        let longest = format!("{{\"{}\":1}}", "k".repeat(MAX_MEMBER_NAME_BYTES));
        assert!(DataValue::parse(&longest).is_ok());
        let too_long = format!("{{\"{}\":1}}", "k".repeat(MAX_MEMBER_NAME_BYTES + 1));
        assert!(DataValue::parse(&too_long).is_err());
    }

    #[test]
    fn nesting_and_size_are_bounded_before_the_stack_is() {
        let deepest = format!("{}{}", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
        assert!(DataValue::parse(&deepest).is_ok());
        let deeper = format!(
            "{}{}",
            "[".repeat(MAX_NESTING + 1),
            "]".repeat(MAX_NESTING + 1)
        );
        assert_eq!(
            DataValue::parse(&deeper),
            Err(DataError::TooLarge(DataLimit::Nesting))
        );
        // Far deeper than any stack would take if the parser recursed
        // without a bound.
        let absurd = "[".repeat(1_000_000);
        assert_eq!(
            DataValue::parse(&absurd),
            Err(DataError::TooLarge(DataLimit::ValueBytes))
        );
        let absurd = "[".repeat(MAX_VALUE_BYTES);
        assert_eq!(
            DataValue::parse(&absurd),
            Err(DataError::TooLarge(DataLimit::Nesting))
        );
        let big = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES));
        assert_eq!(
            DataValue::parse(&big),
            Err(DataError::TooLarge(DataLimit::ValueBytes))
        );
        let fits = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES - 2));
        assert!(DataValue::parse(&fits).is_ok());
    }

    #[test]
    fn get_reads_objects_only_and_a_missing_path_has_nothing() {
        let space = space("{\"a\":{\"b\":[1,2],\"c\":null},\"s\":\"x\"}");
        assert_eq!(space.get(&path("")), Some(space.root()));
        assert_eq!(space.get(&path("a/c")), Some(&DataValue::Null));
        assert_eq!(space.get(&path("a/b")), Some(&value("[1,2]")));
        assert_eq!(space.get(&path("a/missing")), None);
        assert_eq!(space.get(&path("s/x")), None, "through a string");
        assert_eq!(space.get(&path("a/b/0")), None, "paths never enter arrays");
    }

    #[test]
    fn set_replaces_the_subtree_and_makes_the_objects_above() -> TestResult {
        assert_eq!(set("{}", "a/b/c", "1")?, "{\"a\":{\"b\":{\"c\":1}}}");
        assert_eq!(set("{\"a\":{\"x\":1}}", "a", "[1]")?, "{\"a\":[1]}");
        assert_eq!(
            set("{\"a\":{\"x\":1}}", "a/y", "2")?,
            "{\"a\":{\"x\":1,\"y\":2}}"
        );
        assert_eq!(
            set("{\"a\":1}", "a", "{\"b\":null}")?,
            "{\"a\":{\"b\":null}}"
        );
        assert_eq!(
            set("{\"a\":1}", "a", "null")?,
            "{\"a\":null}",
            "set stores null"
        );
        assert_eq!(
            set("{\"a\":1}", "", "{\"z\":1}")?,
            "{\"z\":1}",
            "the root is replaced"
        );
        Ok(())
    }

    #[test]
    fn set_refuses_to_make_room_below_a_value_that_is_not_an_object() {
        for from in [
            "{\"a\":\"s\"}",
            "{\"a\":1}",
            "{\"a\":[1]}",
            "{\"a\":null}",
            "{\"a\":true}",
        ] {
            assert_eq!(
                set(from, "a/b", "1"),
                Err(DataError::PathConflict),
                "{from}"
            );
            assert_eq!(
                set(from, "a/b/c", "1"),
                Err(DataError::PathConflict),
                "{from}"
            );
            assert_eq!(
                patch(from, "a/b", "{\"x\":1}"),
                Err(DataError::PathConflict)
            );
        }
        assert_eq!(
            set("{\"a\":{\"b\":2}}", "a/b/c", "1"),
            Err(DataError::PathConflict)
        );
    }

    #[test]
    fn the_root_stays_an_object() {
        for not_object in ["1", "[]", "null", "\"x\""] {
            assert_eq!(set("{}", "", not_object), Err(DataError::RootNotObject));
            assert_eq!(patch("{}", "", not_object), Err(DataError::RootNotObject));
        }
    }

    #[test]
    fn merge_patch_follows_rfc_7396() -> TestResult {
        // Objects merge member by member.
        assert_eq!(
            patch(
                "{\"a\":{\"x\":1,\"y\":2},\"b\":3}",
                "",
                "{\"a\":{\"y\":9,\"z\":4}}"
            )?,
            "{\"a\":{\"x\":1,\"y\":9,\"z\":4},\"b\":3}"
        );
        // A null member removes; removing what is not there is nothing.
        assert_eq!(
            patch("{\"a\":1,\"b\":2}", "", "{\"a\":null,\"c\":null}")?,
            "{\"b\":2}"
        );
        // Arrays and scalars replace wholesale, also an array of objects.
        assert_eq!(patch("{\"a\":[1,2,3]}", "", "{\"a\":[9]}")?, "{\"a\":[9]}");
        assert_eq!(
            patch("{\"a\":[{\"x\":1}]}", "", "{\"a\":[{\"y\":null}]}")?,
            "{\"a\":[{\"y\":null}]}",
            "nothing inside an array is merged, and its nulls stay"
        );
        assert_eq!(patch("{\"a\":{\"x\":1}}", "", "{\"a\":5}")?, "{\"a\":5}");
        // An object patch over a value that is not an object starts from {}
        // and drops its null members.
        assert_eq!(
            patch("{\"a\":\"s\"}", "", "{\"a\":{\"x\":1,\"y\":null}}")?,
            "{\"a\":{\"x\":1}}"
        );
        assert_eq!(
            patch("{}", "", "{\"a\":{\"b\":{\"c\":null}}}")?,
            "{\"a\":{\"b\":{}}}"
        );
        assert_eq!(patch("{\"a\":[1]}", "", "{\"a\":{}}")?, "{\"a\":{}}");
        Ok(())
    }

    #[test]
    fn merge_at_a_path_is_a_merge_at_the_root_of_the_nested_patch() -> TestResult {
        assert_eq!(
            patch("{\"a\":{\"b\":{\"x\":1}}}", "a/b", "{\"y\":2}")?,
            "{\"a\":{\"b\":{\"x\":1,\"y\":2}}}"
        );
        assert_eq!(
            patch("{}", "a/b", "{\"y\":2}")?,
            "{\"a\":{\"b\":{\"y\":2}}}"
        );
        assert_eq!(
            patch("{\"a\":{\"b\":1}}", "a/b", "[1]")?,
            "{\"a\":{\"b\":[1]}}"
        );
        assert_eq!(patch("{\"a\":{\"b\":1}}", "a/b", "7")?, "{\"a\":{\"b\":7}}");
        // The patch null removes the target.
        assert_eq!(
            patch("{\"a\":{\"b\":1,\"c\":2}}", "a/b", "null")?,
            "{\"a\":{\"c\":2}}"
        );
        // Removing what is not there changes nothing, also below a value
        // that is not an object, and makes nothing on the way.
        assert_eq!(patch("{\"a\":{}}", "a/b", "null")?, "{\"a\":{}}");
        assert_eq!(patch("{\"a\":\"s\"}", "a/b", "null")?, "{\"a\":\"s\"}");
        assert_eq!(patch("{}", "x/y/z", "null")?, "{}");
        // An empty object patch makes the target an object if it is not one.
        assert_eq!(patch("{}", "a", "{}")?, "{\"a\":{}}");
        // A null patch of the root is not a way to remove the root.
        assert_eq!(patch("{}", "", "null"), Err(DataError::RootNotObject));
        Ok(())
    }

    #[test]
    fn merge_patch_cannot_store_null_but_set_can() -> TestResult {
        assert_eq!(patch("{\"a\":1}", "", "{\"a\":null}")?, "{}");
        assert_eq!(set("{}", "a", "null")?, "{\"a\":null}");
        assert_eq!(
            set("{\"a\":null}", "a/b", "1"),
            Err(DataError::PathConflict)
        );
        Ok(())
    }

    #[test]
    fn remove_deletes_the_subtree_and_a_missing_path_is_nothing() -> TestResult {
        assert_eq!(remove("{\"a\":{\"b\":1},\"c\":2}", "a")?, "{\"c\":2}");
        assert_eq!(remove("{\"a\":{\"b\":1}}", "a/b")?, "{\"a\":{}}");
        assert_eq!(remove("{\"a\":1}", "")?, "{}");
        for (from, at) in [
            ("{}", "a"),
            ("{}", "a/b/c"),
            ("{\"a\":\"s\"}", "a/b"),
            ("{\"a\":[1]}", "a/0"),
        ] {
            let applied = space(from).apply(&DataChange::Remove { path: path(at) })?;
            assert!(!applied.changed, "{from} {at}");
            assert_eq!(applied.space, space(from));
        }
        Ok(())
    }

    #[test]
    fn changed_says_whether_the_space_differs() -> TestResult {
        let base = space("{\"a\":{\"b\":[1,2]},\"n\":1}");
        for (change, changed) in [
            (
                DataChange::Set {
                    path: path("n"),
                    value: value("1.0"),
                },
                false,
            ),
            (
                DataChange::Set {
                    path: path("n"),
                    value: value("2"),
                },
                true,
            ),
            (
                DataChange::Set {
                    path: path("a/b"),
                    value: value("[1,2]"),
                },
                false,
            ),
            (
                DataChange::Set {
                    path: path("a/b"),
                    value: value("[2,1]"),
                },
                true,
            ),
            (
                DataChange::Set {
                    path: path("a"),
                    value: value("{\"b\":[1,2]}"),
                },
                false,
            ),
            (
                DataChange::Merge {
                    path: path(""),
                    patch: value("{\"n\":1}"),
                },
                false,
            ),
            (
                DataChange::Merge {
                    path: path(""),
                    patch: value("{\"n\":null}"),
                },
                true,
            ),
            (
                DataChange::Merge {
                    path: path(""),
                    patch: value("{\"zz\":null}"),
                },
                false,
            ),
            (
                DataChange::Remove {
                    path: path("nothing"),
                },
                false,
            ),
            (DataChange::Remove { path: path("n") }, true),
        ] {
            assert_eq!(base.apply(&change)?.changed, changed, "{change:?}");
        }
        Ok(())
    }

    #[test]
    fn a_change_that_would_exceed_a_limit_is_refused() -> TestResult {
        // The count of values: `a` and its items.
        let items = |count: usize| vec!["0"; count].join(",");
        let fits = DataSpace::new(value(&format!(
            "{{\"a\":[{}]}}",
            items(MAX_SPACE_VALUES - 1)
        )))?;
        assert_eq!(fits.value_count(), MAX_SPACE_VALUES);
        assert_eq!(
            DataSpace::new(value(&format!("{{\"a\":[{}]}}", items(MAX_SPACE_VALUES)))),
            Err(DataError::TooLarge(DataLimit::SpaceValues))
        );
        let one_more = DataChange::Set {
            path: path("b"),
            value: value("0"),
        };
        assert_eq!(
            fits.apply(&one_more),
            Err(DataError::TooLarge(DataLimit::SpaceValues))
        );
        // Nesting counts the objects of the path and the value together.
        let arrays = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
        let deep_path = vec!["a"; MAX_PATH_DEPTH].join("/");
        let ok = DataChange::Set {
            path: path(&deep_path),
            value: value(&arrays(MAX_NESTING - MAX_PATH_DEPTH)),
        };
        assert!(DataSpace::empty().apply(&ok).is_ok());
        let too_deep = DataChange::Set {
            path: path(&deep_path),
            value: value(&arrays(MAX_NESTING - MAX_PATH_DEPTH + 1)),
        };
        assert_eq!(
            DataSpace::empty().apply(&too_deep),
            Err(DataError::TooLarge(DataLimit::Nesting))
        );
        // The size of the whole space, with every request within its own bound.
        let chunk = format!("\"{}\"", "x".repeat(250_000));
        let mut grown = DataSpace::empty();
        for name in ["a", "b", "c", "d"] {
            let change = DataChange::Set {
                path: path(name),
                value: value(&chunk),
            };
            grown = grown.apply(&change)?.space;
        }
        let change = DataChange::Set {
            path: path("e"),
            value: value(&chunk),
        };
        assert_eq!(
            grown.apply(&change),
            Err(DataError::TooLarge(DataLimit::SpaceBytes))
        );
        // A refused change leaves the space as it was; one that shrinks is fine.
        let shrink = DataChange::Remove { path: path("a") };
        assert!(grown.apply(&shrink)?.changed);
        Ok(())
    }

    fn query(at: &str, select: MemberSelect, limit: usize, after: Option<&str>) -> MemberQuery {
        MemberQuery::new(path(at), select, limit, after.map(str::to_owned))
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn names(page: &MemberPage) -> Vec<&str> {
        page.members
            .iter()
            .map(|member| member.name.as_str())
            .collect()
    }

    #[test]
    fn members_are_listed_in_utf8_byte_order_not_utf16_order() {
        // U+FF5E is EF BD 9E and U+10000 is F0 90 80 80: their bytes order
        // them one way and their UTF-16 code units (FF5E; D800 DC00) the other.
        let space = space("{\"a\":1,\"B\":2,\"\\uff5e\":3,\"\\ud800\\udc00\":4,\"\\u00e9\":5}");
        let page = space.members(&query("", MemberSelect::Names, 100, None));
        assert_eq!(names(&page), ["B", "a", "\u{e9}", "\u{ff5e}", "\u{10000}"]);
        assert!(
            page.members
                .iter()
                .all(|member| member.value == MemberValue::Omitted)
        );
        assert!(page.exists && page.collection && page.next_after.is_none());
    }

    #[test]
    fn pages_chain_through_next_after_without_gaps_or_repeats() {
        let body: Vec<String> = (0..7).map(|n| format!("\"m{n}\":{n}")).collect();
        let space = space(&format!("{{{}}}", body.join(",")));
        let mut seen: Vec<String> = Vec::new();
        let mut after: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = space.members(&query("", MemberSelect::Names, 3, after.as_deref()));
            assert!(page.members.len() <= 3, "limit exceeded: {page:?}");
            seen.extend(page.members.iter().map(|member| member.name.clone()));
            pages += 1;
            match page.next_after {
                Some(next) => {
                    assert_eq!(
                        Some(&next),
                        seen.last(),
                        "next_after is the last name listed"
                    );
                    after = Some(next);
                }
                None => break,
            }
        }
        assert_eq!(seen, ["m0", "m1", "m2", "m3", "m4", "m5", "m6"]);
        assert_eq!(pages, 3);
    }

    #[test]
    fn after_is_exclusive_and_need_not_be_a_member() {
        let space = space("{\"b\":1,\"d\":2}");
        let page = space.members(&query("", MemberSelect::Names, 10, Some("b")));
        assert_eq!(names(&page), ["d"]);
        let page = space.members(&query("", MemberSelect::Names, 10, Some("c")));
        assert_eq!(names(&page), ["d"]);
        let page = space.members(&query("", MemberSelect::Names, 10, Some("d")));
        assert!(page.members.is_empty() && page.next_after.is_none() && page.collection);
    }

    #[test]
    fn a_value_over_the_member_bound_is_reported_not_carried() {
        let exact = "x".repeat(MEMBER_VALUE_BYTES - 2);
        let over = "x".repeat(MEMBER_VALUE_BYTES - 1);
        let space = space(&format!(
            "{{\"a\":\"{exact}\",\"b\":\"{over}\",\"c\":null}}"
        ));
        let page = space.members(&query("", MemberSelect::Values, 10, None));
        assert_eq!(
            page.members[0].value,
            MemberValue::Value(format!("\"{exact}\""))
        );
        assert_eq!(page.members[1].value, MemberValue::TooLarge);
        assert_eq!(page.members[2].value, MemberValue::Value("null".into()));
    }

    #[test]
    fn a_page_stops_before_the_byte_bound_and_always_lists_one_member() {
        // Each value is 30,000 bytes and its quotes: eight fit in 256 KiB.
        let big = "x".repeat(30_000);
        let body: Vec<String> = (0..10).map(|n| format!("\"m{n}\":\"{big}\"")).collect();
        // Over the limit of one written value, as a space grown by many writes
        // is: the space itself may hold 1 MiB.
        let text = format!("{{{}}}", body.join(","));
        let root = DataValue::parse_unbounded(&text).unwrap_or_else(|error| panic!("{error}"));
        let space = DataSpace::new(root).unwrap_or_else(|error| panic!("{error}"));
        let first = space.members(&query("", MemberSelect::Values, 100, None));
        assert_eq!(first.members.len(), 8);
        assert_eq!(first.next_after.as_deref(), Some("m7"));
        let second = space.members(&query("", MemberSelect::Values, 100, Some("m7")));
        assert_eq!(names(&second), ["m8", "m9"]);
        assert!(second.next_after.is_none());
        // Names alone are never held back by the byte bound at the limit.
        let names_only = space.members(&query("", MemberSelect::Names, 100, None));
        assert_eq!(names_only.members.len(), 10);
    }

    #[test]
    fn a_page_of_exactly_the_byte_bound_is_whole() {
        // Eight members of 32,768 bytes each (a name of 2 bytes and a value of
        // 32,764 characters in quotes) are exactly 256 KiB; a ninth, however
        // small, does not fit.
        let big = "x".repeat(32_764);
        let mut body: Vec<String> = (0..8).map(|n| format!("\"m{n}\":\"{big}\"")).collect();
        body.push("\"m8\":1".to_owned());
        let root = DataValue::parse_unbounded(&format!("{{{}}}", body.join(",")))
            .unwrap_or_else(|error| panic!("{error}"));
        let space = DataSpace::new(root).unwrap_or_else(|error| panic!("{error}"));
        let page = space.members(&query("", MemberSelect::Values, 100, None));
        assert_eq!(page.members.len(), 8);
        assert!(
            page.members
                .iter()
                .all(|member| matches!(member.value, MemberValue::Value(_))),
            "a value of exactly the member bound is carried"
        );
        assert_eq!(page.next_after.as_deref(), Some("m7"));
    }

    #[test]
    fn a_path_that_is_not_an_object_lists_nothing() {
        let space = space("{\"s\":\"text\",\"a\":[1],\"o\":{\"k\":1}}");
        for (at, exists, collection) in [
            ("s", true, false),
            ("a", true, false),
            ("missing", false, false),
            ("s/deeper", false, false),
            ("o", true, true),
            ("", true, true),
        ] {
            let page = space.members(&query(at, MemberSelect::Names, 10, None));
            assert_eq!((page.exists, page.collection), (exists, collection), "{at}");
            assert!(
                page.collection || (page.members.is_empty() && page.next_after.is_none()),
                "{at}"
            );
        }
    }

    #[test]
    fn a_member_query_refuses_what_the_grammar_refuses() {
        let at = DataPath::root();
        for limit in [0, MAX_MEMBER_LIMIT + 1] {
            assert!(
                MemberQuery::new(at.clone(), MemberSelect::Names, limit, None).is_err(),
                "limit {limit}"
            );
        }
        for limit in [1, MAX_MEMBER_LIMIT] {
            assert!(MemberQuery::new(at.clone(), MemberSelect::Names, limit, None).is_ok());
        }
        for after in [String::new(), "x".repeat(MAX_MEMBER_NAME_BYTES + 1)] {
            assert!(MemberQuery::new(at.clone(), MemberSelect::Names, 1, Some(after)).is_err());
        }
        assert!(
            MemberQuery::new(
                at,
                MemberSelect::Names,
                1,
                Some("x".repeat(MAX_MEMBER_NAME_BYTES))
            )
            .is_ok()
        );
    }
}
