//! Local-only disclosure checks. They do not establish tool safety or identity.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disclosure {
    Public,
    Requester,
    Unclassified,
}
fn patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(||[
        r"(?i)[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+",
        r"\+(?:[ ()-]*[0-9]){8,15}(?:[^0-9]|$)",
        r"(?:^|[^0-9])0[0-9]{1,4}[- (][0-9]{1,4}[- )][0-9]{4}(?:[^0-9]|$)",
        r"(?im)(?:^|[\s\{\[\x22'])(?:電話(?:番号)?|tel|phone)\s*[\x22']?\s*[:=]\s*[+0-9 (][0-9 ()-]{6,20}",
        r"(?im)(?:^|[\s\{\[\x22'])(?:住所|address|氏名|full_name|個人番号|マイナンバー)\s*[\x22']?\s*[:=]\s*\S+",
        r"[0-9]{3}-[0-9]{4}\s+[^\n]*(?:都|道|府|県)[^\n]*(?:市|区|町|村)[^\n]*[0-9]",
        r"(?im)(?:^|[\s\{\[\x22'])(?:authorization|proxy_authorization|cookie|set-cookie|password|passwd|api_key|access_token|refresh_token|client_secret)\s*[\x22']?\s*[:=]\s*\S+",
        r"(?i)(?:[?&#]|^)(?:token|access_token|api_key|key|signature|sig|code|x-amz-[a-z-]+|x-goog-[a-z-]+)=[^&#\s]+",
        r"-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----",
    ].into_iter().map(|s|Regex::new(s).expect("constant privacy pattern")).collect())
}
fn decoded(s: &str) -> Result<Option<String>, ()> {
    let input = s.as_bytes();
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut changed = false;
    while index < input.len() {
        if input[index] == b'%'
            && index + 2 < input.len()
            && let (Some(a), Some(b)) = (
                (input[index + 1] as char).to_digit(16),
                (input[index + 2] as char).to_digit(16),
            )
        {
            out.push((a * 16 + b) as u8);
            index += 3;
            changed = true;
        } else {
            out.push(input[index]);
            index += 1;
        }
    }
    if !changed {
        return Ok(None);
    }
    String::from_utf8(out).map(Some).map_err(|_| ())
}
fn sensitive(s: &str, credentials_only: bool) -> bool {
    let skip = if credentials_only { 6 } else { 0 };
    if patterns().iter().skip(skip).any(|r| r.is_match(s)) {
        return true;
    }
    for (pos, _) in s.match_indices("://") {
        if s[pos + 3..]
            .split(['/', '?', '#', ' ', '\n', '\t'])
            .next()
            .is_some_and(|a| a.contains('@'))
        {
            return true;
        }
    }
    for word in s.split(|c: char| !c.is_ascii_alphanumeric() && !"-_.".contains(c)) {
        let parts: Vec<_> = word.split('.').collect();
        if parts.len() == 3
            && parts.iter().all(|s| !s.is_empty())
            && let Ok(bytes) = URL_SAFE_NO_PAD.decode(parts[0])
            && serde_json::from_slice::<Value>(&bytes).is_ok_and(|v| v.is_object())
        {
            return true;
        }
    }
    false
}
pub fn text(value: &str) -> Disclosure {
    inspect(value, false)
}
fn inspect(value: &str, credentials_only: bool) -> Disclosure {
    if value.len() > 262_144 {
        return Disclosure::Unclassified;
    }
    let mut normalized: String = value.nfkc().collect();
    for step in 0..=4 {
        if normalized.len()>1_048_576 || normalized.chars().any(|c| (c.is_control() && !['\n','\r','\t'].contains(&c)) || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')) {
            return Disclosure::Unclassified;
        }
        if sensitive(&normalized, credentials_only) {
            return Disclosure::Requester;
        }
        match decoded(&normalized) {
            Ok(None) => return Disclosure::Public,
            Ok(Some(_)) if step == 4 => return Disclosure::Unclassified,
            Ok(Some(next)) => normalized = next.nfkc().collect(),
            Err(()) => return Disclosure::Unclassified,
        }
    }
    Disclosure::Unclassified
}
/// Only call for an evaluated ordinary-input schema. Unknown purpose belongs
/// to Unclassified at the registry layer, even when this scan finds no secrets.
pub fn ordinary_arguments(value: &Value) -> Disclosure {
    match value {
        Value::String(s) => text(s),
        Value::Array(a) => combine(a.iter().map(ordinary_arguments)),
        Value::Object(o) => combine(o.iter().map(|(k, v)| {
            let key: String = k.nfkc().flat_map(char::to_lowercase).collect();
            if [
                "email",
                "phone",
                "address",
                "full_name",
                "user_id",
                "password",
                "token",
                "api_key",
                "cookie",
                "authorization",
                "client_secret",
            ]
            .contains(&key.as_str())
                && !v.is_null()
                && v != &Value::String(String::new())
                && v != &serde_json::json!([])
            {
                Disclosure::Requester
            } else {
                combine([text(k), ordinary_arguments(v)].into_iter())
            }
        })),
        _ => Disclosure::Public,
    }
}
/// Conservative credential check, separate from private contact disclosure.
/// An uninspectable string cannot be covered by a turn-scoped grant.
pub fn credential_free(value: &Value) -> bool {
    match value {
        Value::String(s) => inspect(s, true) == Disclosure::Public,
        Value::Array(a) => a.iter().all(credential_free),
        Value::Object(o) => o.iter().all(|(k, v)| {
            let key: String = k.nfkc().flat_map(char::to_lowercase).collect();
            ![
                "password",
                "passwd",
                "token",
                "api_key",
                "cookie",
                "authorization",
                "client_secret",
                "access_token",
                "refresh_token",
            ]
            .contains(&key.as_str())
                && inspect(k, true) == Disclosure::Public
                && credential_free(v)
        }),
        _ => true,
    }
}
fn combine(values: impl Iterator<Item = Disclosure>) -> Disclosure {
    values.fold(Disclosure::Public, |a, b| match (a, b) {
        (Disclosure::Requester, _) | (_, Disclosure::Requester) => Disclosure::Requester,
        (Disclosure::Unclassified, _) | (_, Disclosure::Unclassified) => Disclosure::Unclassified,
        _ => Disclosure::Public,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agreed_privacy_examples() {
        let cases: Value =
            serde_json::from_str(include_str!("../../docs/mcp-approval/privacy-cases.json"))
                .unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let context = case["context"].as_str().unwrap();
            let actual = match context {
                "gmail.search"
                | "family.setting"
                | "evaluated_contact"
                | "evaluated_person_id"
                | "evaluated_executable_code" => Disclosure::Requester,
                "ordinary_text" | "ordinary_url" | "evaluated_document_id" => {
                    text(case["input"].as_str().unwrap())
                }
                _ => Disclosure::Unclassified,
            };
            let expected = match case["expected"].as_str().unwrap() {
                "public" => Disclosure::Public,
                "requester" => Disclosure::Requester,
                _ => Disclosure::Unclassified,
            };
            assert_eq!(actual, expected, "{}", case["id"]);
        }
    }
    #[test]
    fn private_contact_does_not_mean_credentials_but_encoded_secrets_do() {
        let contact = serde_json::json!({"text":"alice@example.com"});
        assert_eq!(ordinary_arguments(&contact), Disclosure::Requester);
        assert!(credential_free(&contact));
        for secret in [
            "https://example.com/?token=secret",
            "password: secret",
            "https://user:secret@example.com/",
            "%2570assword%253A%2520secret",
            "https://example.com/%FF",
            "hello\u{202e}world",
        ] {
            assert!(
                !credential_free(&serde_json::json!({"text":secret})),
                "{secret}"
            );
        }
        assert!(!credential_free(
            &serde_json::json!({"nested":{"password":"value"}})
        ));
    }
    #[test]
    fn nested_contact_and_secret_keys_are_private() {
        assert_eq!(
            ordinary_arguments(&serde_json::json!({"fields":[{"password":"test-only"}]})),
            Disclosure::Requester
        );
        assert_eq!(
            ordinary_arguments(&serde_json::json!({"text":"普通の文章","count":3})),
            Disclosure::Public
        );
    }
}
