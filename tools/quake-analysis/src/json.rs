//! A small JSON value with the layout the reports use: two-space indent and
//! object keys in sorted order.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::num::float_repr;

/// A JSON value. Integers and floats stay distinct so that `50` and `50.0`
/// print differently.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// An integer.
    Int(i128),
    /// A floating point number.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object; the map keeps keys sorted.
    Obj(BTreeMap<String, Json>),
}

impl Json {
    /// Build an object from `(key, value)` pairs.
    pub fn obj<I, K>(pairs: I) -> Json
    where
        I: IntoIterator<Item = (K, Json)>,
        K: Into<String>,
    {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Look up `key` in an object. Panics if this is not an object holding it.
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(map) => map
                .get(key)
                .unwrap_or_else(|| panic!("missing JSON key {key}")),
            _ => panic!("not a JSON object"),
        }
    }

    /// The integer held by this value.
    pub fn int(&self, key: &str) -> i128 {
        match self.get(key) {
            Json::Int(value) => *value,
            other => panic!("{key} is not an integer: {other:?}"),
        }
    }

    /// The float held by this value.
    pub fn float(&self, key: &str) -> f64 {
        match self.get(key) {
            Json::Float(value) => *value,
            other => panic!("{key} is not a float: {other:?}"),
        }
    }

    /// The string held by this value.
    pub fn text(&self, key: &str) -> &str {
        match self.get(key) {
            Json::Str(value) => value,
            other => panic!("{key} is not a string: {other:?}"),
        }
    }

    /// Serialize with a two-space indent. No trailing newline.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, level: usize) {
        match self {
            Json::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Json::Float(value) => {
                if value.is_nan() {
                    out.push_str("NaN");
                } else if value.is_infinite() {
                    out.push_str(if *value < 0.0 {
                        "-Infinity"
                    } else {
                        "Infinity"
                    });
                } else {
                    out.push_str(&float_repr(*value));
                }
            }
            Json::Str(value) => write_string(out, value),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    indent(out, level + 1);
                    item.write(out, level + 1);
                }
                out.push('\n');
                indent(out, level);
                out.push(']');
            }
            Json::Obj(map) if map.is_empty() => out.push_str("{}"),
            Json::Obj(map) => {
                out.push_str("{\n");
                for (index, (key, value)) in map.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    indent(out, level + 1);
                    write_string(out, key);
                    out.push_str(": ");
                    value.write(out, level + 1);
                }
                out.push('\n');
                indent(out, level);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::Json;

    #[test]
    fn layout_matches_indented_sorted_dump() {
        let value = Json::obj([
            ("b", Json::Arr(vec![Json::Int(1), Json::Float(2.0)])),
            ("a", Json::obj([("x", Json::Str("q\"é".into()))])),
            ("e", Json::Arr(vec![])),
            ("o", Json::obj(Vec::<(String, Json)>::new())),
        ]);
        let expected = "{\n  \"a\": {\n    \"x\": \"q\\\"\\u00e9\"\n  },\n  \"b\": [\n    1,\n    2.0\n  ],\n  \"e\": [],\n  \"o\": {}\n}";
        assert_eq!(value.pretty(), expected);
    }
}
