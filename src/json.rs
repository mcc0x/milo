//! Validates JSON and borrows values from the input. Skipped values allocate nothing.

use std::borrow::Cow;
use std::ops::Range;

const REPORT_DEPTH: usize = 32;
const SETTINGS_DEPTH: usize = 129; // The root object and up to 128 nested containers.

/// Settings edits preserve unknown values and whitespace byte for byte.
pub struct Member<'a> {
    pub key: Value<'a>,
    pub value: Value<'a>,
    pub key_range: Range<usize>,
    pub value_range: Range<usize>,
}

/// Visit only the top-level members, returning the offset just after the opening brace.
pub fn settings_object<'a>(text: &'a str, mut visit: impl FnMut(Member<'a>)) -> Option<usize> {
    let mut input = Input::<true>(text);
    input.space();
    let open = text.len() - input.0.len() + 1;
    input.0.starts_with('{').then_some(())?;
    input.collection(1, |key, value, key_tail, value_tail| {
        let key_start = text.len() - key_tail;
        let value_start = text.len() - value_tail;
        visit(Member {
            key,
            value,
            key_range: key_start..key_start + key.0.len(),
            value_range: value_start..value_start + value.0.len(),
        });
    })?;
    input.space();
    input.0.is_empty().then_some(open)
}

#[derive(Clone, Copy)]
pub struct Value<'a>(&'a str);

impl<'a> Value<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let mut input = Input::<false>(std::str::from_utf8(bytes).ok()?);
        let value = input.value(0)?;
        input.space();
        input.0.is_empty().then_some(value)
    }

    pub fn fields<const N: usize>(self, names: [&str; N]) -> [Option<Self>; N] {
        let mut values = [None; N];
        if self.object() {
            let _ = Input::<false>(self.0).collection(0, |key, value, _, _| {
                if let Some(key) = key.string(64)
                    && let Some(i) = names.iter().position(|name| *name == key)
                {
                    values[i] = Some(value);
                }
            });
        }
        values
    }

    pub fn object(self) -> bool {
        self.0.starts_with('{')
    }

    pub fn null(self) -> bool {
        self.0 == "null"
    }

    pub fn boolean(self) -> Option<bool> {
        match self.0 {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    pub fn elements(self) -> Option<impl Iterator<Item = Self>> {
        let mut input = Input::<false>(self.0.strip_prefix('[')?);
        Some(std::iter::from_fn(move || {
            let value = input.value(0)?;
            input.space();
            input.eat(',');
            Some(value)
        }))
    }

    /// Compare short fixed names without allocating even when they are escaped.
    pub fn is(self, mut expected: &str) -> bool {
        let Some(text) = self.0.strip_prefix('"').and_then(|text| text.strip_suffix('"')) else { return false };
        if text.len() > expected.len().saturating_mul(6) {
            return false;
        }
        let mut input = Input::<false>(text);
        while let Some((plain, escaped)) = input.0.split_once('\\') {
            input.0 = escaped;
            let Some(tail) = expected.strip_prefix(plain).and_then(|tail| tail.strip_prefix(input.escape()?)) else {
                return false;
            };
            expected = tail;
        }
        input.0 == expected
    }

    pub fn string(self, max_bytes: usize) -> Option<Cow<'a, str>> {
        let text = self.0.strip_prefix('"')?.strip_suffix('"')?;
        if !text.contains('\\') {
            return (text.len() <= max_bytes).then_some(Cow::Borrowed(text));
        }
        if text.len() > max_bytes.saturating_mul(6) {
            return None;
        }
        let mut decoded = String::with_capacity(text.len().min(max_bytes));
        let mut input = Input::<false>(text);
        while let Some((plain, escaped)) = input.0.split_once('\\') {
            decoded.push_str(plain);
            input.0 = escaped;
            decoded.push(input.escape()?);
        }
        decoded.push_str(input.0);
        (decoded.len() <= max_bytes).then_some(Cow::Owned(decoded))
    }

    pub fn unsigned(self) -> Option<u64> {
        self.0.parse().ok()
    }

    pub fn integer(self) -> Option<i64> {
        self.0.parse().ok()
    }

    pub fn number(self) -> Option<f64> {
        let number = self.0.parse::<f64>().ok()?;
        number.is_finite().then_some(number)
    }
}

/// A settings edit only needs valid syntax, but reports need decodable strings.
struct Input<'a, const SETTINGS: bool>(&'a str);

impl<'a, const SETTINGS: bool> Input<'a, SETTINGS> {
    fn space(&mut self) {
        self.0 = self.0.trim_start_matches([' ', '\t', '\r', '\n']);
    }

    fn eat(&mut self, expected: char) -> bool {
        if let Some(rest) = self.0.strip_prefix(expected) {
            self.0 = rest;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value<'a>> {
        self.space();
        let limit = if SETTINGS { SETTINGS_DEPTH } else { REPORT_DEPTH };
        let start = self.0;
        match start.as_bytes().first()? {
            b'"' => {
                self.string()?;
            }
            b'{' | b'[' if depth < limit => self.collection(depth + 1, |_, _, _, _| {})?,
            b'-' | b'0'..=b'9' => self.number()?,
            b't' => self.0 = start.strip_prefix("true")?,
            b'f' => self.0 = start.strip_prefix("false")?,
            b'n' => self.0 = start.strip_prefix("null")?,
            _ => return None,
        }
        Some(Value(&start[..start.len() - self.0.len()]))
    }

    fn collection(&mut self, depth: usize, mut visit: impl FnMut(Value<'a>, Value<'a>, usize, usize)) -> Option<()> {
        let object = self.eat('{');
        if !object {
            self.eat('[');
        }
        let end = if object { '}' } else { ']' };
        self.space();
        if self.eat(end) {
            return Some(());
        }
        loop {
            self.space();
            if object {
                let key_tail = self.0.len();
                let key = self.string()?;
                self.space();
                self.eat(':').then_some(())?;
                self.space();
                let value_tail = self.0.len();
                let value = self.value(depth)?;
                visit(key, value, key_tail, value_tail);
            } else {
                self.value(depth)?;
            }
            self.space();
            if self.eat(end) {
                return Some(());
            }
            self.eat(',').then_some(())?;
        }
    }

    fn string(&mut self) -> Option<Value<'a>> {
        let start = self.0;
        self.eat('"').then_some(())?;
        loop {
            // ASCII delimiters are always on character boundaries in UTF-8.
            let end = self.0.bytes().position(|b| matches!(b, b'"' | b'\\' | 0..=31))?;
            self.0 = &self.0[end..];
            if self.eat('"') {
                return Some(Value(&start[..start.len() - self.0.len()]));
            }
            self.eat('\\').then_some(())?;
            if SETTINGS && self.eat('u') {
                self.hex()?;
            } else {
                self.escape()?;
            }
        }
    }

    fn escape(&mut self) -> Option<char> {
        let code = *self.0.as_bytes().first()?;
        if !code.is_ascii() {
            return None;
        }
        self.0 = &self.0[1..];
        Some(match code {
            b'"' | b'\\' | b'/' => char::from(code),
            b'b' => '\x08',
            b'f' => '\x0c',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let scalar = match self.hex()? {
                    high @ 0xd800..=0xdbff => {
                        self.0 = self.0.strip_prefix("\\u")?;
                        let low = self.hex()?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return None;
                        }
                        0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                    }
                    0xdc00..=0xdfff => return None,
                    scalar => scalar,
                };
                char::from_u32(scalar)?
            }
            _ => return None,
        })
    }

    fn hex(&mut self) -> Option<u32> {
        let digits = self.0.get(..4)?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.0 = &self.0[4..];
        u32::from_str_radix(digits, 16).ok()
    }

    fn digits(&mut self) -> bool {
        let start = self.0.len();
        self.0 = self.0.trim_start_matches(|c: char| c.is_ascii_digit());
        start != self.0.len()
    }

    fn number(&mut self) -> Option<()> {
        self.eat('-');
        if !self.eat('0') && !self.digits() {
            return None;
        }
        if self.eat('.') && !self.digits() {
            return None;
        }
        if self.eat('e') || self.eat('E') {
            if !self.eat('+') {
                self.eat('-');
            }
            self.digits().then_some(())?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_accept_leading_json_whitespace_and_keep_edit_offsets() {
        let text = " \t\r\n{ \"other\" : [null] }\r\n";
        let mut members = 0;
        let open = settings_object(text, |member| {
            members += 1;
            assert_eq!(&text[member.key_range], "\"other\"");
            assert_eq!(&text[member.value_range], "[null]");
        });
        assert_eq!(open, Some(5));
        assert_eq!(members, 1);
        assert!(settings_object("\u{a0}{}", |_| {}).is_none());
    }

    #[test]
    fn settings_and_reports_keep_their_own_nesting_bounds() {
        let array = |depth: usize, value: &str| format!("{}{value}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(Value::parse(array(32, "0").as_bytes()).is_some());
        assert!(Value::parse(array(33, "0").as_bytes()).is_none());

        let settings = |depth, value| format!("{{\"other\":{}}}", array(depth, value));
        for inner in ["", "0"] {
            assert!(settings_object(&settings(128, inner), |_| {}).is_some());
            assert!(settings_object(&settings(129, inner), |_| {}).is_none());
        }
    }

    #[test]
    fn an_out_of_range_number_does_not_hide_other_fields() {
        let huge = "9".repeat(100_000);
        let report = format!("{{\"impossible\":{huge},\"maximum\":{},\"next\":42}}", u64::MAX);
        let value = Value::parse(report.as_bytes()).unwrap();
        let [impossible, maximum, next] = value.fields(["impossible", "maximum", "next"]);
        let impossible = impossible.unwrap();
        assert_eq!((impossible.integer(), impossible.unsigned(), impossible.number()), (None, None, None));
        assert_eq!(maximum.unwrap().unsigned(), Some(u64::MAX));
        assert_eq!(next.unwrap().integer(), Some(42));
    }
}
