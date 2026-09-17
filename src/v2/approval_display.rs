//! Lossless, typed argument projection. Rendering confers no semantic approval.
use serde_json::{Value, json};

pub const ARGUMENT_BYTES: usize = 262_144;
pub const TOTAL_BYTES: usize = 262_144;
pub const PAGE_UNITS: usize = 4_800;
pub const PAGE_FIELDS: usize = 24;
pub const MAX_PAGES: usize = 64;
const CHUNK_UNITS: usize = 1_800;
pub const UNREVIEWED: &str =
    "Codexが求めた操作と入力値です。Proxyでは操作の意味・安全性を評価していません";

pub fn units(v: &Value) -> usize {
    match v {
        Value::String(s) => s.encode_utf16().count(),
        Value::Array(a) => a.iter().map(units).sum(),
        Value::Object(o) => o.values().map(units).sum(),
        _ => 0,
    }
}
fn chunks(value: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut count = 0;
    for (index, ch) in value.char_indices() {
        if count + ch.len_utf16() > CHUNK_UNITS {
            out.push(&value[start..index]);
            start = index;
            count = 0;
        }
        count += ch.len_utf16();
    }
    out.push(&value[start..]);
    out
}
fn field(
    path: &str,
    value: &Value,
    fields: &mut Vec<Value>,
    budget: &mut usize,
) -> Result<(), &'static str> {
    let (kind, text) = match value {
        Value::Object(o) => ("object", format!("オブジェクト：{}項目", o.len())),
        Value::Array(a) => ("array", format!("配列：{}要素（順序どおり）", a.len())),
        Value::String(_) => ("string", value.to_string()),
        Value::Number(_) => ("number", value.to_string()),
        Value::Bool(_) => ("boolean", value.to_string()),
        Value::Null => ("null", "null".into()),
    };
    let parts = chunks(&text);
    for (index, part) in parts.iter().enumerate() {
        let label = format!(
            "{}（{}）{}",
            if path.is_empty() {
                "引数全体"
            } else {
                path
            },
            kind,
            if parts.len() > 1 {
                format!("［分割 {}/{}］", index + 1, parts.len())
            } else {
                String::new()
            }
        );
        let entry = json!({"label":label,"value":part});
        let bytes = serde_json::to_vec(&entry)
            .map_err(|_| "arguments_invalid")?
            .len();
        *budget = budget.checked_sub(bytes).ok_or("information_incomplete")?;
        fields.push(entry);
    }
    match value {
        Value::Object(o) => {
            for (key, value) in o {
                field(
                    &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                    value,
                    fields,
                    budget,
                )?;
            }
        }
        Value::Array(a) => {
            for (index, value) in a.iter().enumerate() {
                field(&format!("{path}/{index}"), value, fields, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Every page includes the disclosure and explanation, making detached pages
/// interpretable. A caller must bind ALL pages before accepting a reply.
pub fn raw_pages(server: &str, tool: &str, arguments: &Value) -> Result<Vec<Value>, &'static str> {
    pages(server, tool, arguments, None)
}
pub fn pages(
    server: &str,
    tool: &str,
    arguments: &Value,
    description: Option<&str>,
) -> Result<Vec<Value>, &'static str> {
    if serde_json::to_vec(arguments)
        .map_err(|_| "arguments_invalid")?
        .len()
        > ARGUMENT_BYTES
    {
        return Err("arguments_too_large");
    }
    let mut fields = vec![json!({"label":"接続先／操作","value":format!("{server} / {tool}")})];
    let mut budget = TOTAL_BYTES;
    field("", arguments, &mut fields, &mut budget)?;
    if description.is_some() && server == "playwright" {
        let labels: &[(&str, &str)] = match tool {
            "browser_find" => &[("/text", "検索語"), ("/regex", "正規表現")],
            "browser_navigate" => &[("/url", "アクセス先URL")],
            "browser_tabs" => &[("/action", "タブの操作")],
            _ => &[],
        };
        for field in &mut fields {
            if let Some(label) = field["label"].as_str()
                && let Some((_, japanese)) = labels
                    .iter()
                    .find(|(path, _)| label.starts_with(&format!("{path}（")))
            {
                field["label"] = json!(format!("{japanese} {label}"));
            }
        }
    }
    let base = json!({"disclosure":"requester_only","provenance":"proxy_verified_call","title":"MCP操作の確認","fields":[],"limitations":[description.unwrap_or(UNREVIEWED),"値はJSON表記です。引用符・改行等のescapeと分割順を保持しています"],"omissions":[]});
    let mut pages = vec![];
    let mut current = base.clone();
    for f in fields {
        if units(&base) + units(&f) > PAGE_UNITS {
            return Err("information_incomplete");
        }
        let full = current["fields"].as_array().unwrap().len() == PAGE_FIELDS
            || units(&current) + units(&f) > PAGE_UNITS;
        if full {
            pages.push(current);
            current = base.clone();
        }
        current["fields"].as_array_mut().unwrap().push(f);
        if pages.len() >= MAX_PAGES {
            return Err("information_incomplete");
        }
    }
    pages.push(current);
    if serde_json::to_vec(&pages)
        .map_err(|_| "arguments_invalid")?
        .len()
        > TOTAL_BYTES
    {
        return Err("information_incomplete");
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_structure_types_empty_values_and_pointer_escaping() {
        let value = json!({"a/b~c":[null,false,{},[],"",9007199254740993u64]});
        let pages = raw_pages("s", "t", &value).unwrap();
        let fields = pages[0]["fields"].as_array().unwrap();
        let text = fields
            .iter()
            .map(|f| format!("{}: {}", f["label"], f["value"]))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("/a~1b~0c/0（null）"));
        assert!(text.contains("/a~1b~0c/1（boolean）"));
        assert!(text.contains("9007199254740993"));
        assert!(text.contains("オブジェクト：0項目"));
        assert!(text.contains("配列：0要素"));
    }
    #[test]
    fn long_unicode_is_losslessly_split_and_every_page_obeys_limits() {
        let value = json!({"text":"絵😀\n\"".repeat(2000)});
        let pages = raw_pages("s", "t", &value).unwrap();
        assert!(pages.len() > 1);
        let mut encoded = String::new();
        for page in &pages {
            assert!(units(page) <= PAGE_UNITS);
            assert!(page["fields"].as_array().unwrap().len() <= PAGE_FIELDS);
            for field in page["fields"].as_array().unwrap() {
                if field["label"]
                    .as_str()
                    .unwrap()
                    .starts_with("/text（string）")
                {
                    encoded.push_str(field["value"].as_str().unwrap());
                }
            }
        }
        assert_eq!(
            serde_json::from_str::<Value>(&encoded).unwrap(),
            value["text"]
        );
    }
    #[test]
    fn unrepresentable_display_is_rejected_instead_of_truncated() {
        assert_eq!(
            raw_pages("s", "t", &json!({"x":"x".repeat(ARGUMENT_BYTES)})).unwrap_err(),
            "arguments_too_large"
        );
        let mut value = serde_json::Map::new();
        value.insert("x".repeat(PAGE_UNITS), json!(true));
        assert_eq!(
            raw_pages("s", "t", &Value::Object(value)).unwrap_err(),
            "information_incomplete"
        );
    }
}
