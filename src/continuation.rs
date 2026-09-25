//! Opaque continuation tokens for progressive / paged MCP tool results.
//!
//! When a tool hits `token_budget`, it appends a `continuation:` footer. The
//! next call with that token resumes the same query from `offset` without
//! re-emitting earlier rows. Tokens are self-contained (base64url JSON); no
//! DiskMap required.

use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const VERSION: u32 = 1;

/// Tool-specific resume kind (what `offset` counts).
pub const KIND_FILE_SKIP: &str = "file_skip";
pub const KIND_HIT_SKIP: &str = "hit_skip";
pub const KIND_ITEM_SKIP: &str = "item_skip";
pub const KIND_CALLER_SKIP: &str = "caller_skip";
pub const KIND_MODULE_SKIP: &str = "module_skip";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub tool: String,
    pub args_fp: u64,
    pub offset: u64,
    pub kind: String,
}

/// Stable fingerprint of the query args that define a walk (order-independent
/// for object keys we care about).
pub fn args_fingerprint(args: &Value, keys: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for key in keys {
        if let Some(v) = args.get(*key) {
            if *key == "continuation" {
                continue;
            }
            pairs.push(((*key).to_string(), stable_json(v)));
        }
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in pairs {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }
    hasher.finish()
}

fn stable_json(v: &Value) -> String {
    match v {
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_json).collect();
            // Preserve array order (paths/files order matters for walks).
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", k, stable_json(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

pub fn encode(tool: &str, args_fp: u64, offset: u64, kind: &str) -> String {
    let payload = json!({
        "v": VERSION,
        "tool": tool,
        "args_fp": args_fp,
        "offset": offset,
        "kind": kind,
    });
    let raw = serde_json::to_vec(&payload).unwrap_or_default();
    b64url_encode(&raw)
}

pub fn decode(token: &str) -> Result<Token, String> {
    let raw = b64url_decode(token.trim()).map_err(|e| format!("bad continuation token: {e}"))?;
    let v: Value = serde_json::from_slice(&raw).map_err(|e| format!("bad continuation json: {e}"))?;
    let ver = v.get("v").and_then(Value::as_u64).unwrap_or(0);
    if ver != VERSION as u64 {
        return Err(format!("unsupported continuation version {ver}"));
    }
    let tool = v
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if tool.is_empty() {
        return Err("continuation missing tool".into());
    }
    let args_fp = v
        .get("args_fp")
        .and_then(Value::as_u64)
        .ok_or_else(|| "continuation missing args_fp".to_string())?;
    let offset = v.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let kind = v
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or(KIND_ITEM_SKIP)
        .to_string();
    Ok(Token {
        tool,
        args_fp,
        offset,
        kind,
    })
}

/// Read `continuation` from args, verify tool + fingerprint, return resume offset.
pub fn resume_offset(
    args: &Value,
    tool: &str,
    fingerprint_keys: &[&str],
    expected_kind: &str,
) -> Result<u64, String> {
    let Some(raw) = args.get("continuation").and_then(Value::as_str) else {
        return Ok(0);
    };
    if raw.trim().is_empty() {
        return Ok(0);
    }
    let tok = decode(raw)?;
    if tok.tool != tool {
        return Err(format!(
            "continuation is for tool \"{}\", not \"{tool}\"",
            tok.tool
        ));
    }
    if tok.kind != expected_kind {
        return Err(format!(
            "continuation kind \"{}\" unexpected (want \"{expected_kind}\")",
            tok.kind
        ));
    }
    let fp = args_fingerprint(args, fingerprint_keys);
    if tok.args_fp != fp {
        return Err(
            "continuation does not match current args (paths/profile/mode/pattern/query changed)"
                .into(),
        );
    }
    Ok(tok.offset)
}

/// Append truncation + continuation footer. Keeps the `truncated by token_budget`
/// phrase so stats classification still works.
pub fn append_footer(out: &mut String, tool: &str, args_fp: u64, offset: u64, kind: &str, omitted: &str) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if omitted.is_empty() {
        out.push_str("… (truncated by token_budget)\n");
    } else {
        out.push_str(&format!("… (truncated by token_budget; {omitted})\n"));
    }
    let token = encode(tool, args_fp, offset, kind);
    out.push_str(&format!("continuation: {token}\n"));
    out.push_str("hint: re-call with continuation:\"…\" to fetch the next page (same budget)\n");
}

/// Schema fragment for tools that support paging.
pub fn schema_prop() -> Value {
    json!({
        "type": "string",
        "description": "Opaque token from a prior truncated reply's `continuation:` line; resumes the same query from the skip offset."
    })
}

// --- base64url (no extra crate) ------------------------------------------------

fn b64url_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = (data[i] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
    } else if rem == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
    }
    out
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u32, String> {
        Ok(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'-' => 62,
            b'_' => 63,
            b'=' => return Err("unexpected padding".into()),
            _ => return Err(format!("invalid base64url char {:?}", c as char)),
        })
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i < bytes.len() {
        let remaining = bytes.len() - i;
        if remaining == 1 {
            return Err("truncated base64url".into());
        }
        let a = val(bytes[i])?;
        let b = val(bytes[i + 1])?;
        if remaining == 2 {
            out.push(((a << 2) | (b >> 4)) as u8);
            break;
        }
        let c = val(bytes[i + 2])?;
        if remaining == 3 {
            out.push(((a << 2) | (b >> 4)) as u8);
            out.push((((b & 0xf) << 4) | (c >> 2)) as u8);
            break;
        }
        let d = val(bytes[i + 3])?;
        out.push(((a << 2) | (b >> 4)) as u8);
        out.push((((b & 0xf) << 4) | (c >> 2)) as u8);
        out.push((((c & 0x3) << 6) | d) as u8);
        i += 4;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_token() {
        let t = encode("repo_map", 42, 17, KIND_FILE_SKIP);
        let d = decode(&t).unwrap();
        assert_eq!(d.tool, "repo_map");
        assert_eq!(d.args_fp, 42);
        assert_eq!(d.offset, 17);
        assert_eq!(d.kind, KIND_FILE_SKIP);
    }

    #[test]
    fn fingerprint_ignores_continuation_key_when_listed() {
        let a = json!({"paths": ["src"], "token_budget": 100, "continuation": "x"});
        let b = json!({"paths": ["src"], "token_budget": 100});
        let keys = &["paths", "token_budget", "continuation"];
        // args_fingerprint skips the continuation key always
        assert_eq!(args_fingerprint(&a, keys), args_fingerprint(&b, keys));
    }

    #[test]
    fn resume_rejects_tool_mismatch() {
        let fp = args_fingerprint(&json!({"paths": ["src"]}), &["paths"]);
        let tok = encode("repo_map", fp, 3, KIND_FILE_SKIP);
        let args = json!({"paths": ["src"], "continuation": tok});
        let err = resume_offset(&args, "outline", &["paths"], KIND_FILE_SKIP).unwrap_err();
        assert!(err.contains("repo_map"));
    }

    #[test]
    fn resume_rejects_args_drift() {
        let fp = args_fingerprint(&json!({"paths": ["src"]}), &["paths"]);
        let tok = encode("repo_map", fp, 3, KIND_FILE_SKIP);
        let args = json!({"paths": ["tools"], "continuation": tok});
        assert!(resume_offset(&args, "repo_map", &["paths"], KIND_FILE_SKIP).is_err());
    }

    #[test]
    fn footer_contains_markers() {
        let mut out = String::from("body\n");
        append_footer(&mut out, "repo_map", 1, 5, KIND_FILE_SKIP, "+3 file(s) omitted");
        assert!(out.contains("truncated by token_budget"));
        assert!(out.contains("continuation: "));
        assert!(out.contains("hint: re-call with continuation"));
    }
}
