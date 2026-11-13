//! Read the text of a derived `Debug` (`{:?}`) as JSON. Vixen's generated types derive
//! `Debug` and nothing that serializes.
//!
//! - `Name { a: x }` becomes `{"Name": {"a": x}}`, since a struct and an enum variant print alike.
//! - `Name { kind: x }` becomes `{"kind": x}`: Shipstern and Vixen write an enum as a struct
//!   with one `kind` field, and idl-verify does not look for it under a type name.
//! - `inf`, `-inf` and `NaN` become `null`, as serde writes them.
//! - `Name(x, y)` becomes `{"Name": {"0": x, "1": y}}`; `Some(x)` and `KeyBytes("..")` become `x`.
//! - a bare `Name` is the string `"Name"`, a unit variant; `None` is `null`.
//! - a map `{k: v}` becomes `[[k, v]]`, as idl-verify writes maps.

use serde_json::{Map, Value};

pub(crate) fn to_json(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        chars: text.chars().collect(),
        at: 0,
    };
    let result = p.value().and_then(|v| {
        p.space();
        if p.at == p.chars.len() {
            Ok(v)
        } else {
            Err(format!("Debug text has trailing input at {}", p.at))
        }
    });
    // Show where the reader stopped, so an unexpected print form can be reported.
    result.map_err(|e| {
        let from = p.at.saturating_sub(30);
        let to = (p.at + 30).min(p.chars.len());
        let near: String = p.chars[from..to].iter().collect();
        format!("{e}, near `{near}`")
    })
}

/// JSON text for a number token: wide integers become quoted decimal strings.
fn number_json(number: &str) -> String {
    let integer = !number.contains(['.', 'e', 'E']);
    let fits = number.parse::<i64>().is_ok() || number.parse::<u64>().is_ok();
    if integer && !fits {
        format!("\"{number}\"")
    } else {
        number.to_string()
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.space();
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{c}` at {} in Debug text", self.at))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.space();
        match self.peek() {
            Some('"') => self.string().map(Value::String),
            Some('\'') => self.char_literal().map(Value::String),
            Some('[') => {
                self.at += 1;
                Ok(Value::Array(self.items(']')?))
            }
            Some('(') => {
                self.at += 1;
                Ok(indexed(self.items(')')?))
            }
            Some('{') => {
                self.at += 1;
                self.map()
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) if c.is_alphabetic() || c == '_' => self.named(),
            other => Err(format!("unexpected {other:?} at {} in Debug text", self.at)),
        }
    }

    /// Comma-separated values up to `close`; a trailing comma is allowed.
    fn items(&mut self, close: char) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        loop {
            if self.eat(close) {
                return Ok(out);
            }
            out.push(self.value()?);
            if !self.eat(',') {
                self.expect(close)?;
                return Ok(out);
            }
        }
    }

    fn map(&mut self) -> Result<Value, String> {
        let mut pairs = Vec::new();
        loop {
            if self.eat('}') {
                return Ok(Value::Array(pairs));
            }
            let k = self.value()?;
            self.expect(':')?;
            let v = self.value()?;
            pairs.push(Value::Array(vec![k, v]));
            if !self.eat(',') {
                self.expect('}')?;
                return Ok(Value::Array(pairs));
            }
        }
    }

    fn ident(&mut self) -> String {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect()
    }

    fn named(&mut self) -> Result<Value, String> {
        let name = self.ident();
        self.space();
        // A keyword only when nothing follows: Shipstern has a variant `None(..)`.
        if !matches!(self.peek(), Some('(' | '{')) {
            match name.as_str() {
                "true" => return Ok(Value::Bool(true)),
                "false" => return Ok(Value::Bool(false)),
                // serde writes the floats `inf` and `NaN` as null too.
                "None" | "inf" | "NaN" => return Ok(Value::Null),
                _ => {}
            }
        }
        match self.peek() {
            Some('{') => {
                self.at += 1;
                let mut fields = Map::new();
                loop {
                    if self.eat('}') {
                        break;
                    }
                    self.space();
                    let field = self.ident();
                    self.expect(':')?;
                    fields.insert(field, self.value()?);
                    if !self.eat(',') {
                        self.expect('}')?;
                        break;
                    }
                }
                if fields.len() == 1 && fields.contains_key("kind") {
                    return Ok(Value::Object(fields));
                }
                Ok(wrap(name, Value::Object(fields)))
            }
            Some('(') => {
                self.at += 1;
                let mut items = self.items(')')?;
                if (name == "Some" || name == "KeyBytes") && items.len() == 1 {
                    return Ok(items.remove(0));
                }
                Ok(wrap(name, indexed(items)))
            }
            _ => Ok(Value::String(name)),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        if self.peek() == Some('-') {
            self.at += 1;
            if self.chars[self.at..].starts_with(&['i', 'n', 'f']) {
                self.at += 3;
                return Ok(Value::Null);
            }
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        {
            self.at += 1;
        }
        let token: String = self.chars[start..self.at].iter().collect();
        serde_json::from_str(&number_json(&token)).map_err(|e| format!("number `{token}`: {e}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string in Debug text")?;
            self.at += 1;
            match c {
                '"' => return Ok(out),
                '\\' => out.push(self.escape()?),
                c => out.push(c),
            }
        }
    }

    fn char_literal(&mut self) -> Result<String, String> {
        self.at += 1;
        let c = match self.peek().ok_or("unterminated char in Debug text")? {
            '\\' => {
                self.at += 1;
                self.escape()?
            }
            c => {
                self.at += 1;
                c
            }
        };
        self.expect('\'')?;
        Ok(c.to_string())
    }

    fn escape(&mut self) -> Result<char, String> {
        let c = self.peek().ok_or("unterminated escape in Debug text")?;
        self.at += 1;
        Ok(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            'u' => {
                self.expect('{')?;
                let start = self.at;
                while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.at += 1;
                }
                let hex: String = self.chars[start..self.at].iter().collect();
                self.expect('}')?;
                u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("bad escape \\u{{{hex}}}"))?
            }
            other => other,
        })
    }
}

fn wrap(name: String, body: Value) -> Value {
    let mut m = Map::new();
    m.insert(name, body);
    Value::Object(m)
}

fn indexed(items: Vec<Value>) -> Value {
    Value::Object(
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn derived_debug_reads_as_json() {
        #[derive(Debug)]
        #[allow(dead_code)]
        enum Swap {
            Raydium,
            Whirlpool { a_to_b: bool },
            Pair(u8, i64),
        }
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Step {
            swap: Swap,
            percent: u8,
            note: Option<String>,
            wide: u128,
            map: BTreeMap<u8, bool>,
        }
        let step = Step {
            swap: Swap::Whirlpool { a_to_b: true },
            percent: 100,
            note: Some("a \"b\"\n".into()),
            wide: u128::MAX,
            map: [(1, false)].into(),
        };
        assert_eq!(
            to_json(&format!("{step:?}")).unwrap(),
            json!({"Step": {
                "swap": {"Whirlpool": {"a_to_b": true}},
                "percent": 100,
                "note": "a \"b\"\n",
                "wide": u128::MAX.to_string(),
                "map": [[1, false]],
            }})
        );
        assert_eq!(
            to_json(&format!("{:?}", Swap::Raydium)).unwrap(),
            json!("Raydium")
        );
        assert_eq!(
            to_json(&format!("{:?}", Swap::Pair(1, -2))).unwrap(),
            json!({"Pair": {"0": 1, "1": -2}})
        );
        assert_eq!(
            to_json(&format!("{:?}", [None, Some(3u8)])).unwrap(),
            json!([null, 3])
        );
    }

    #[test]
    fn a_variant_named_none_is_not_null() {
        assert_eq!(
            to_json("R { kind: None(RNone), x: None }").unwrap(),
            json!({"R": {"kind": {"None": {"0": "RNone"}}, "x": null}})
        );
    }

    #[test]
    fn floats_keep_their_text_and_non_finite_values_are_null() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct F {
            a: f32,
            b: f64,
            c: f32,
            d: f64,
            e: f32,
        }
        let f = F {
            a: 0.1,
            b: -2.5e-8,
            c: f32::INFINITY,
            d: f64::NEG_INFINITY,
            e: f32::NAN,
        };
        // serde writes a non-finite float as null, and idl-verify accepts that.
        assert_eq!(
            to_json(&format!("{f:?}")).unwrap(),
            json!({"F": {"a": 0.1, "b": -2.5e-8, "c": null, "d": null, "e": null}})
        );
    }

    #[test]
    fn malformed_text_is_an_error() {
        assert!(to_json("Step { a: 1").is_err());
        assert!(to_json("[1, 2] extra").is_err());
    }
}
