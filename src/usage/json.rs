//! Validates JSON and borrows values from the input. Skipped values allocate nothing.

use std::borrow::Cow;

const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy)]
pub(super) struct Value<'a>(&'a str);

impl<'a> Value<'a> {
    pub(super) fn parse(bytes: &'a [u8]) -> Option<Self> {
        let mut input = Input(std::str::from_utf8(bytes).ok()?);
        let value = input.value(0)?;
        input.space();
        input.0.is_empty().then_some(value)
    }

    pub(super) fn fields<const N: usize>(self, names: [&str; N]) -> [Option<Self>; N] {
        let mut values = [None; N];
        let Some(object) = self.0.strip_prefix('{') else { return values };
        let mut input = Input(object);
        loop {
            input.space();
            let Some(key) = input.string() else { break };
            input.space();
            input.eat(':');
            let Some(value) = input.value(0) else { break };
            if let Some(key) = key.string(64)
                && let Some(i) = names.iter().position(|name| *name == key)
            {
                values[i] = Some(value);
            }
            input.space();
            if !input.eat(',') {
                break;
            }
        }
        values
    }

    pub(super) fn object(self) -> bool {
        self.0.starts_with('{')
    }

    pub(super) fn is(self, text: &str) -> bool {
        self.string(text.len()).is_some_and(|s| s == text)
    }

    pub(super) fn string(self, max_bytes: usize) -> Option<Cow<'a, str>> {
        let text = self.0.strip_prefix('"')?.strip_suffix('"')?;
        if !text.contains('\\') {
            return (text.len() <= max_bytes).then_some(Cow::Borrowed(text));
        }
        if text.len() > max_bytes.saturating_mul(6) {
            return None;
        }
        let mut decoded = String::with_capacity(text.len().min(max_bytes));
        let mut input = Input(text);
        while let Some((plain, escaped)) = input.0.split_once('\\') {
            decoded.push_str(plain);
            input.0 = escaped;
            decoded.push(input.escape()?);
        }
        decoded.push_str(input.0);
        (decoded.len() <= max_bytes).then_some(Cow::Owned(decoded))
    }

    pub(super) fn unsigned(self) -> Option<u64> {
        self.0.parse().ok()
    }

    pub(super) fn integer(self) -> Option<i64> {
        self.0.parse().ok()
    }

    pub(super) fn number(self) -> Option<f64> {
        let number = self.0.parse::<f64>().ok()?;
        number.is_finite().then_some(number)
    }
}

struct Input<'a>(&'a str);

impl<'a> Input<'a> {
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
        let start = self.0;
        match start.as_bytes().first()? {
            b'"' => {
                self.string()?;
            }
            b'{' | b'[' if depth < MAX_DEPTH => self.collection(depth + 1)?,
            b'-' | b'0'..=b'9' => self.number()?,
            b't' => self.0 = start.strip_prefix("true")?,
            b'f' => self.0 = start.strip_prefix("false")?,
            b'n' => self.0 = start.strip_prefix("null")?,
            _ => return None,
        }
        Some(Value(&start[..start.len() - self.0.len()]))
    }

    fn collection(&mut self, depth: usize) -> Option<()> {
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
                self.string()?;
                self.space();
                self.eat(':').then_some(())?;
            }
            self.value(depth)?;
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
            self.escape()?;
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
                let high = self.hex()?;
                if (0xd800..=0xdbff).contains(&high) {
                    self.0 = self.0.strip_prefix("\\u")?;
                    let low = self.hex()?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return None;
                    }
                    char::from_u32(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00))?
                } else {
                    char::from_u32(high)?
                }
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
