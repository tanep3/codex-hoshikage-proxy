//! Bounded JSONL framing and catalog decoding. No tool semantics live here.
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(super) const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
pub(super) const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;

pub(super) async fn frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if out.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "incomplete JSONL frame",
                ))
            };
        }
        let end = available.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(available.len());
        if count > limit.saturating_sub(out.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "JSONL frame too large",
            ));
        }
        out.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(Some(out));
        }
    }
}

struct Budget {
    nodes: usize,
    strings: usize,
}
pub(super) fn catalog(raw: &str) -> Result<Value, &'static str> {
    if raw.len() > MAX_CATALOG_BYTES {
        return Err("catalog_too_large");
    }
    let mut budget = Budget {
        nodes: 262_144,
        strings: MAX_CATALOG_BYTES,
    };
    exact_value(raw, &mut budget, 0).map_err(|_| "catalog_invalid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn frame_boundaries_and_truncation() {
        for size in [7, 8, 9] {
            let input = format!("{}\nnext\n", "x".repeat(size));
            let mut reader = BufReader::with_capacity(3, input.as_bytes());
            let result = frame(&mut reader, 8).await;
            if size > 8 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().unwrap().len(), size);
                assert_eq!(frame(&mut reader, 8).await.unwrap().unwrap(), b"next");
                assert!(frame(&mut reader, 8).await.unwrap().is_none());
            }
        }
        assert!(frame(&mut b"unterminated".as_slice(), 32).await.is_err());
    }

    #[test]
    fn catalog_byte_boundaries() {
        for size in [
            1_100_000,
            MAX_CATALOG_BYTES - 1,
            MAX_CATALOG_BYTES,
            MAX_CATALOG_BYTES + 1,
        ] {
            let raw = format!("\"{}\"", "x".repeat(size - 2));
            assert_eq!(catalog(&raw).is_ok(), size <= MAX_CATALOG_BYTES);
        }
    }

    #[test]
    fn catalog_rejects_duplicate_keys_and_excess_structure() {
        assert!(catalog(r#"{"tool":1,"tool":2}"#).is_err());
        assert!(catalog(r#"{"tool":1,"\u0074ool":2}"#).is_err());
        for depth in [64, 65] {
            let raw = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
            assert_eq!(catalog(&raw).is_ok(), depth == 64);
        }
        for nodes in [262_144, 262_145] {
            let raw = format!("[{}]", vec!["0"; nodes - 1].join(","));
            assert_eq!(catalog(&raw).is_ok(), nodes == 262_144);
        }
        assert!(catalog("null true").is_err());
    }
}

/// Decode call arguments without rounding JSON numbers or silently collapsing
/// duplicate keys. Explicit map parsing also avoids arbitrary_precision's
/// reserved Number key treating an ordinary object as a numeric literal.
pub(super) fn exact_json(raw: &str) -> Result<Value, &'static str> {
    let mut budget = Budget {
        nodes: 262_144,
        strings: MAX_CATALOG_BYTES,
    };
    exact_value(raw, &mut budget, 0)
}
fn exact_value(raw: &str, budget: &mut Budget, depth: usize) -> Result<Value, &'static str> {
    if depth > 64 || budget.nodes == 0 {
        return Err("arguments_invalid");
    }
    budget.nodes -= 1;
    let raw = raw.trim();
    match raw.as_bytes().first() {
        Some(b'{') => {
            struct Object<'a> {
                budget: &'a mut Budget,
                depth: usize,
            }
            impl<'de> Visitor<'de> for Object<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("JSON object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                    let mut out = serde_json::Map::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if out.contains_key(&key) {
                            return Err(de::Error::custom("duplicate argument key"));
                        }
                        self.budget.strings = self
                            .budget
                            .strings
                            .checked_sub(key.len())
                            .ok_or_else(|| de::Error::custom("argument budget"))?;
                        let raw = map.next_value::<&serde_json::value::RawValue>()?;
                        let value = exact_value(raw.get(), self.budget, self.depth + 1)
                            .map_err(de::Error::custom)?;
                        out.insert(key, value);
                    }
                    Ok(Value::Object(out))
                }
            }
            let mut d = serde_json::Deserializer::from_str(raw);
            let value = serde::Deserializer::deserialize_map(&mut d, Object { budget, depth })
                .map_err(|_| "arguments_invalid")?;
            d.end().map_err(|_| "arguments_invalid")?;
            Ok(value)
        }
        Some(b'[') => {
            struct Array<'a> {
                budget: &'a mut Budget,
                depth: usize,
            }
            impl<'de> Visitor<'de> for Array<'_> {
                type Value = Value;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("JSON array")
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                    let mut out = Vec::new();
                    while let Some(raw) = seq.next_element::<&serde_json::value::RawValue>()? {
                        out.push(
                            exact_value(raw.get(), self.budget, self.depth + 1)
                                .map_err(de::Error::custom)?,
                        );
                    }
                    Ok(Value::Array(out))
                }
            }
            let mut d = serde_json::Deserializer::from_str(raw);
            let value = serde::Deserializer::deserialize_seq(&mut d, Array { budget, depth })
                .map_err(|_| "arguments_invalid")?;
            d.end().map_err(|_| "arguments_invalid")?;
            Ok(value)
        }
        Some(b'"') => {
            let s: String = serde_json::from_str(raw).map_err(|_| "arguments_invalid")?;
            budget.strings = budget
                .strings
                .checked_sub(s.len())
                .ok_or("arguments_invalid")?;
            Ok(Value::String(s))
        }
        Some(b't' | b'f') => serde_json::from_str::<bool>(raw)
            .map(Value::Bool)
            .map_err(|_| "arguments_invalid"),
        Some(b'n') if raw == "null" => Ok(Value::Null),
        Some(b'-' | b'0'..=b'9') => raw
            .parse::<serde_json::Number>()
            .map(Value::Number)
            .map_err(|_| "arguments_invalid"),
        _ => Err("arguments_invalid"),
    }
}

#[cfg(test)]
mod exact_tests {
    use super::*;
    #[test]
    fn precision_and_object_types_are_preserved() {
        let raw = r#"{"integer":123456789012345678901234567890,"decimal":0.123456789012345678901,"literal":{"$serde_json::private::Number":"42"}}"#;
        let v = exact_json(raw).unwrap();
        assert_eq!(v["integer"].to_string(), "123456789012345678901234567890");
        assert_eq!(v["decimal"].to_string(), "0.123456789012345678901");
        assert!(v["literal"].is_object());
        assert_eq!(v["literal"]["$serde_json::private::Number"], "42");
        assert!(exact_json(r#"{"a":1,"\u0061":2}"#).is_err());
        assert!(exact_json(r#"{"a":[{"x":0,"x":1}]}"#).is_err());
    }
}
