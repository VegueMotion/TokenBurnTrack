use std::fmt;
use std::str::FromStr;

use serde_json::Value;
use tbt_core::Decimal;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    Field(String),
    Index(i64),
}

#[derive(Debug, Clone)]
pub struct JsonPath {
    source: String,
    segments: Vec<Segment>,
}

impl PartialEq for JsonPath {
    fn eq(&self, other: &Self) -> bool {
        self.segments == other.segments
    }
}

impl Eq for JsonPath {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid path `{path}` at offset {offset}: {message}")]
pub struct PathError {
    pub path: String,
    pub offset: usize,
    pub message: String,
}

impl JsonPath {
    pub fn parse(src: &str) -> Result<JsonPath, PathError> {
        let (path, end) = JsonPath::parse_prefix(src)?;
        match src[end..].chars().next() {
            None => Ok(path),
            Some(c) => Err(error(src, end, format!("unexpected `{c}`"))),
        }
    }

    pub fn parse_prefix(src: &str) -> Result<(JsonPath, usize), PathError> {
        let mut p = Parser { src, pos: 0 };
        if !p.eat('$') {
            return Err(error(src, 0, "a path must start with `$`"));
        }
        let mut segments = Vec::new();
        loop {
            if p.eat('.') {
                segments.push(Segment::Field(p.ident()?));
            } else if p.eat('[') {
                segments.push(p.bracket()?);
            } else {
                break;
            }
        }
        let path = JsonPath {
            source: src[..p.pos].to_owned(),
            segments,
        };
        Ok((path, p.pos))
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn get<'v>(&self, root: &'v Value) -> Option<&'v Value> {
        let mut cur = root;
        for seg in &self.segments {
            cur = match (seg, cur) {
                (Segment::Field(name), Value::Object(map)) => map.get(name)?,
                (Segment::Index(i), Value::Array(items)) => {
                    let idx = if *i < 0 {
                        items
                            .len()
                            .checked_sub(usize::try_from(i.unsigned_abs()).ok()?)?
                    } else {
                        usize::try_from(*i).ok()?
                    };
                    items.get(idx)?
                }
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn get_present<'v>(&self, root: &'v Value) -> Option<&'v Value> {
        self.get(root).filter(|v| !v.is_null())
    }
}

impl FromStr for JsonPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<JsonPath, PathError> {
        JsonPath::parse(s)
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

fn error(src: &str, offset: usize, message: impl Into<String>) -> PathError {
    PathError {
        path: src.to_owned(),
        offset,
        message: message.into(),
    }
}

struct Parser<'s> {
    src: &'s str,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn err(&self, message: impl Into<String>) -> PathError {
        error(self.src, self.pos, message)
    }

    fn ident(&mut self) -> Result<String, PathError> {
        let start = self.pos;
        while let Some(c) = self.peek()
            && (c.is_alphanumeric() || matches!(c, '_' | '-' | '$' | '@'))
        {
            self.pos += c.len_utf8();
        }
        if self.pos == start {
            return Err(self.err(match self.peek() {
                Some(c) => format!(
                    "expected a field name after `.`, found `{c}` (use [\"...\"] for other names)"
                ),
                None => "expected a field name after `.`".to_owned(),
            }));
        }
        Ok(self.src[start..self.pos].to_owned())
    }

    fn bracket(&mut self) -> Result<Segment, PathError> {
        let seg = match self.peek() {
            Some(q @ ('"' | '\'')) => {
                self.pos += 1;
                Segment::Field(self.quoted(q)?)
            }
            Some(c) if c == '-' || c.is_ascii_digit() => Segment::Index(self.index()?),
            Some('*') => return Err(self.err("wildcards are not supported")),
            _ => return Err(self.err("expected a quoted field name or an index after `[`")),
        };
        if !self.eat(']') {
            return Err(self.err("expected `]`"));
        }
        Ok(seg)
    }

    fn quoted(&mut self, quote: char) -> Result<String, PathError> {
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err(format!(
                    "unterminated quoted field name, expected `{quote}`"
                )));
            };
            self.pos += c.len_utf8();
            match c {
                '\\' => match self.peek() {
                    Some(e @ ('\\' | '"' | '\'')) => {
                        self.pos += 1;
                        out.push(e);
                    }
                    _ => return Err(self.err("only \\\\, \\\" and \\' escapes are supported")),
                },
                c if c == quote => return Ok(out),
                c => out.push(c),
            }
        }
    }

    fn index(&mut self) -> Result<i64, PathError> {
        let start = self.pos;
        self.eat('-');
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        text.parse()
            .map_err(|_| error(self.src, start, format!("invalid index `{text}`")))
    }
}

pub fn as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let d = as_decimal(v)?;
    if (d.is_sign_negative() && !d.is_zero()) || !d.fract().is_zero() {
        return None;
    }
    u64::try_from(d).ok()
}

pub fn as_decimal(v: &Value) -> Option<Decimal> {
    let text = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_owned(),
        _ => return None,
    };
    Decimal::from_str(&text)
        .or_else(|_| Decimal::from_scientific(&text))
        .ok()
}

pub fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get<'v>(path: &str, v: &'v Value) -> Option<&'v Value> {
        JsonPath::parse(path).unwrap().get(v)
    }

    #[test]
    fn parses_all_segment_kinds() {
        let p = JsonPath::parse(r#"$.a["b c"]['d\'e'][0][-1].f_g-h"#).unwrap();
        assert_eq!(
            p.segments(),
            [
                Segment::Field("a".into()),
                Segment::Field("b c".into()),
                Segment::Field("d'e".into()),
                Segment::Index(0),
                Segment::Index(-1),
                Segment::Field("f_g-h".into()),
            ]
        );
        assert_eq!(p.to_string(), r#"$.a["b c"]['d\'e'][0][-1].f_g-h"#);
        assert!(JsonPath::parse("$").unwrap().segments().is_empty());
        assert_eq!(
            JsonPath::parse("$.a").unwrap(),
            JsonPath::parse(r#"$["a"]"#).unwrap()
        );
    }

    #[test]
    fn evaluates() {
        let v = json!({
            "usage": {"prompt_tokens": 10, "details": [{"n": 1}, {"n": 2}]},
            "odd key": "x",
            "nil": null
        });
        assert_eq!(get("$.usage.prompt_tokens", &v), Some(&json!(10)));
        assert_eq!(get(r#"$["odd key"]"#, &v), Some(&json!("x")));
        assert_eq!(get("$.usage.details[0].n", &v), Some(&json!(1)));
        assert_eq!(get("$.usage.details[-1].n", &v), Some(&json!(2)));
        assert_eq!(get("$.usage.details[-3]", &v), None);
        assert_eq!(get("$.usage.details[2]", &v), None);
        assert_eq!(get("$.usage.details.n", &v), None);
        assert_eq!(get("$.usage[0]", &v), None);
        assert_eq!(get("$.missing.deeper", &v), None);
        assert_eq!(get("$", &v), Some(&v));
        assert_eq!(get("$.nil", &v), Some(&Value::Null));
        let nil = JsonPath::parse("$.nil").unwrap();
        assert_eq!(nil.get_present(&v), None);
    }

    #[test]
    fn errors_name_path_and_offset() {
        let cases = [
            ("usage.x", 0, "a path must start with `$`"),
            ("$.", 2, "expected a field name after `.`"),
            (
                "$..a",
                2,
                "expected a field name after `.`, found `.` (use [\"...\"] for other names)",
            ),
            (
                "$.a[",
                4,
                "expected a quoted field name or an index after `[`",
            ),
            ("$.a[*]", 4, "wildcards are not supported"),
            ("$.a[1", 5, "expected `]`"),
            (
                r#"$["a"#,
                4,
                "unterminated quoted field name, expected `\"`",
            ),
            (
                r#"$["a\n"]"#,
                5,
                "only \\\\, \\\" and \\' escapes are supported",
            ),
            ("$[-]", 2, "invalid index `-`"),
            ("$.a b", 3, "unexpected ` `"),
            ("$.a]", 3, "unexpected `]`"),
        ];
        for (path, offset, message) in cases {
            let e = JsonPath::parse(path).unwrap_err();
            assert_eq!((e.offset, e.message.as_str()), (offset, message), "{path}");
            assert_eq!(e.path, path);
        }
        assert_eq!(
            JsonPath::parse("$.a[").unwrap_err().to_string(),
            "invalid path `$.a[` at offset 4: expected a quoted field name or an index after `[`"
        );
    }

    #[test]
    fn parse_prefix_stops_at_first_foreign_char() {
        let (p, end) = JsonPath::parse_prefix(r#"$.message.id}:{$.requestId}"#).unwrap();
        assert_eq!((p.as_str(), end), ("$.message.id", 12));
        let (p, end) = JsonPath::parse_prefix(r#"$.type == "usage""#).unwrap();
        assert_eq!((p.as_str(), end), ("$.type", 6));
    }

    #[test]
    fn numbers_from_json_numbers_and_strings() {
        assert_eq!(as_u64(&json!(42)), Some(42));
        assert_eq!(as_u64(&json!("42")), Some(42));
        assert_eq!(as_u64(&json!(" 7 ")), Some(7));
        assert_eq!(as_u64(&json!(12.0)), Some(12));
        assert_eq!(as_u64(&json!("1e3")), Some(1000));
        assert_eq!(as_u64(&json!(12.5)), None);
        assert_eq!(as_u64(&json!(-1)), None);
        assert_eq!(as_u64(&json!("abc")), None);
        assert_eq!(as_u64(&json!(true)), None);
        assert_eq!(as_decimal(&json!(0.0123)), Some("0.0123".parse().unwrap()));
        assert_eq!(as_decimal(&json!("0.5")), Some("0.5".parse().unwrap()));
        assert_eq!(
            as_decimal(&json!(1.5e-5)),
            Some("0.000015".parse().unwrap())
        );
        assert_eq!(as_text(&json!(17)), Some("17".into()));
        assert_eq!(as_text(&json!("")), None);
        assert_eq!(as_text(&json!({"a": 1})), None);
    }
}
