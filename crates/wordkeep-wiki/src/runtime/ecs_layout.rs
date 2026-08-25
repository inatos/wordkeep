//! Confirm-gated POD field reorder (large → small) for C++ structs.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Field {
    pub ty: String,
    pub name: String,
    pub est_size: u32,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub struct_name: String,
    pub fields: Vec<Field>,
    pub reordered: Vec<Field>,
    pub changed: bool,
}

pub fn estimate_size(ty: &str) -> u32 {
    let t = ty.trim().replace("const ", "").replace("unsigned ", "u");
    let t = t.as_str();
    if t.contains('*') || t.ends_with("&") {
        return 8;
    }
    match t {
        "bool" | "char" | "int8_t" | "uint8_t" | "i8" | "u8" => 1,
        "int16_t" | "uint16_t" | "short" => 2,
        "int" | "int32_t" | "uint32_t" | "float" | "i32" | "u32" => 4,
        "int64_t" | "uint64_t" | "double" | "size_t" | "i64" | "u64" | "flecs::entity" => 8,
        "Vec2" => 8,
        "Vec3" => 12,
        "Vec4" | "Quat" => 16,
        _ => 4,
    }
}

pub fn parse_struct<'a>(src: &'a str, name: &str) -> Result<(usize, usize, Vec<Field>), String> {
    let needle = format!("struct {name}");
    let start = src
        .find(&needle)
        .ok_or_else(|| format!("struct {name} not found"))?;
    let brace = src[start..]
        .find('{')
        .ok_or_else(|| format!("struct {name}: missing body"))?;
    let body_open = start + brace;
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in src[body_open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(body_open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let body_close = end.ok_or_else(|| format!("struct {name}: unclosed"))?;
    let inner = &src[body_open + 1..body_close];
    let mut fields = Vec::new();
    for line in inner.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('#')
        {
            continue;
        }
        if line.starts_with("static") || line.contains('(') {
            continue;
        }
        let line = line.trim_end_matches(';').trim();
        if line.is_empty() {
            continue;
        }
        // type name [= init]
        let decl = line.split('=').next().unwrap_or(line).trim();
        let mut toks: Vec<&str> = decl.split_whitespace().collect();
        if toks.len() < 2 {
            continue;
        }
        let fname = toks.pop().unwrap().trim_start_matches('*');
        let ty = toks.join(" ");
        fields.push(Field {
            est_size: estimate_size(&ty),
            ty,
            name: fname.to_string(),
        });
    }
    if fields.is_empty() {
        return Err(format!("struct {name}: no POD fields parsed"));
    }
    Ok((body_open + 1, body_close, fields))
}

pub fn reorder(fields: &[Field]) -> Vec<Field> {
    let mut v = fields.to_vec();
    v.sort_by(|a, b| {
        b.est_size
            .cmp(&a.est_size)
            .then_with(|| a.name.cmp(&b.name))
    });
    v
}

pub fn suggest(src: &str, name: &str) -> Result<Suggestion, String> {
    let (_, _, fields) = parse_struct(src, name)?;
    let reordered = reorder(&fields);
    let changed = reordered
        .iter()
        .map(|f| f.name.as_str())
        .ne(fields.iter().map(|f| f.name.as_str()));
    Ok(Suggestion {
        struct_name: name.to_string(),
        fields,
        reordered,
        changed,
    })
}

pub fn apply_src(src: &str, name: &str) -> Result<String, String> {
    let (open, close, fields) = parse_struct(src, name)?;
    validate_rewrite_body(&src[open..close], fields.len())?;
    let reordered = reorder(&fields);
    if reordered
        .iter()
        .map(|f| f.name.as_str())
        .eq(fields.iter().map(|f| f.name.as_str()))
    {
        return Err("already packed (no field reorder)".into());
    }
    let mut body = String::from("\n");
    for f in &reordered {
        body.push_str(&format!("    {} {};\n", f.ty, f.name));
    }
    let mut out = String::new();
    out.push_str(&src[..open]);
    out.push_str(&body);
    out.push_str(&src[close..]);
    Ok(out)
}

fn validate_rewrite_body(body: &str, parsed_fields: usize) -> Result<(), String> {
    let mut declarations = 0usize;
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('#')
            || line.starts_with("static_assert")
        {
            return Err(
                "apply only supports plain POD fields; comments/directives/static_assert in the body must be moved or edited manually"
                    .into(),
            );
        }
        let lower = line.to_ascii_lowercase();
        if [
            "std::",
            "string",
            "vector",
            "map<",
            "set<",
            "function<",
            "unique_ptr",
            "shared_ptr",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            return Err(format!("apply refused non-POD-looking field `{line}`"));
        }
        if line.contains("//")
            || line.contains("/*")
            || line.contains('=')
            || line.contains('(')
            || line.contains('{')
            || line.contains('}')
            || line.contains('*')
            || line.contains('&')
            || line.contains(',')
            || line.contains(':')
            || !line.ends_with(';')
        {
            return Err(format!(
                "apply refused unsupported declaration `{line}`; only one plain POD field per line is safe"
            ));
        }
        declarations += 1;
    }
    if declarations != parsed_fields {
        return Err("apply parser coverage mismatch; no rewrite performed".into());
    }
    Ok(())
}

pub fn validate_under_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.trim().is_empty()
        || rel.contains('\0')
        || rel.contains('\\')
        || rel.contains("..")
        || Path::new(rel).is_absolute()
    {
        return Err("path must be a relative workspace file".into());
    }
    if !(rel.ends_with(".h") || rel.ends_with(".hpp") || rel.ends_with(".cpp")) {
        return Err("ECS apply is limited to C/C++ headers and sources".into());
    }
    let joined = root.join(rel);
    let canon = joined.canonicalize().unwrap_or(joined.clone());
    let root_c = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canon.starts_with(&root_c) {
        return Err("path escapes workspace".into());
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
struct PackedWrong {
    bool a;
    double b;
    uint8_t c;
    float d;
};
"#;

    #[test]
    fn reorders_large_to_small() {
        let s = suggest(SRC, "PackedWrong").unwrap();
        assert!(s.changed);
        assert_eq!(s.reordered[0].name, "b");
        let out = apply_src(SRC, "PackedWrong").unwrap();
        assert!(out.find("double b").unwrap() < out.find("bool a").unwrap());
    }

    #[test]
    fn missing_struct_errors() {
        assert!(suggest(SRC, "Nope").is_err());
    }

    #[test]
    fn apply_refuses_content_it_would_not_preserve() {
        let commented = "struct X {\n  // owner\n  bool a;\n  uint64_t b;\n};\n";
        assert!(apply_src(commented, "X").unwrap_err().contains("comments"));

        let initialized = "struct X {\n  bool a = false;\n  uint64_t b;\n};\n";
        assert!(apply_src(initialized, "X")
            .unwrap_err()
            .contains("unsupported declaration"));

        let container = "struct X {\n  bool a;\n  std::vector<int> values;\n};\n";
        assert!(apply_src(container, "X").unwrap_err().contains("non-POD"));
    }
}
