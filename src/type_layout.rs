//! type_layout - the fields of a struct/class/union, flagging non-POD members.
//!
//! Finds a named record (C/C++ struct/class/union or a Rust struct) and lists
//! its fields in declaration order with type + name, marking members that look
//! non-trivial (std::string, containers, smart pointers, std::function, or a
//! virtual method; Rust String/Vec/Box/Rc/Arc/HashMap). This pairs with the
//! "keep components POD" rule: a quick check before adding a field to a
//! hot-path record type, without opening the whole header.

use serde_json::Value;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

use crate::lang::Lang;
use crate::{stats, walk};

struct Field {
    ty: String,
    name: String,
    non_pod: Option<String>, // Some(marker) when heuristically non-POD
}

struct Layout {
    rel: String,
    line: usize,
    kind: String, // struct | class | union
    fields: Vec<Field>,
    polymorphic: bool,
}

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let ty = args
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if ty.is_empty() {
        return Err("type is required".into());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let (block, scanned_bytes) = layout_block(root, ty, &paths, budget);
    let out = block.unwrap_or_else(|| {
        format!("type_layout - \"{ty}\": no struct/class/union with that name under {paths:?}")
    });
    stats::record("type_layout", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// Find the record named `ty` under `paths` and render its field layout, along
/// with the number of bytes scanned. Returns `None` when no struct/class/union
/// with that name is found. Shared with `symbol_context`, which folds a record's
/// layout into its composed payload.
pub fn layout_block(
    root: &Path,
    ty: &str,
    paths: &[String],
    budget: usize,
) -> (Option<String>, u64) {
    let prune = walk::prune_set(root);
    let mut parser = Parser::new();
    let mut scanned_bytes = 0u64;
    let mut found: Option<Layout> = None;

    'outer: for p in paths {
        let base = root.join(p);
        // Deterministic order so "first match wins" is stable across runs.
        let mut files: Vec<PathBuf> = walk::files(&base, &prune)
            .filter_map(|e| {
                let path = e.path().to_path_buf();
                matches!(Lang::from_path(&path), Some(Lang::Cpp) | Some(Lang::Rust)).then_some(path)
            })
            .collect();
        files.sort();
        for path in files {
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Cheap prefilter: the name must literally appear to define it here.
            if !src.contains(ty) {
                continue;
            }
            scanned_bytes += src.len() as u64;
            let lang = Lang::from_path(&path).unwrap();
            if let Some(mut layout) = find_layout(&mut parser, lang, &src, ty) {
                layout.rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                found = Some(layout);
                break 'outer;
            }
        }
    }

    (found.map(|l| render(ty, &l, budget)), scanned_bytes)
}

fn find_layout(parser: &mut Parser, lang: Lang, src: &str, ty: &str) -> Option<Layout> {
    let tsl = lang.ts_language()?;
    parser.set_language(&tsl).ok()?;
    let tree = parser.parse(src, None)?;
    let bytes = src.as_bytes();
    match lang {
        Lang::Cpp => find_cpp_record(tree.root_node(), bytes, ty),
        Lang::Rust => find_rust_struct(tree.root_node(), bytes, ty),
        _ => None,
    }
}

// --- C/C++ ----------------------------------------------------------------

fn find_cpp_record(node: Node, bytes: &[u8], ty: &str) -> Option<Layout> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "struct_specifier" | "class_specifier" | "union_specifier"
        ) {
            if let (Some(name), Some(body)) = (
                child.child_by_field_name("name"),
                child.child_by_field_name("body"),
            ) {
                if text(name, bytes) == ty {
                    let kind = match child.kind() {
                        "class_specifier" => "class",
                        "union_specifier" => "union",
                        _ => "struct",
                    }
                    .to_string();
                    let (fields, polymorphic) = cpp_fields(body, bytes);
                    return Some(Layout {
                        rel: String::new(),
                        line: child.start_position().row + 1,
                        kind,
                        fields,
                        polymorphic,
                    });
                }
            }
        }
        if let Some(found) = find_cpp_record(child, bytes, ty) {
            return Some(found);
        }
    }
    None
}

fn cpp_fields(body: Node, bytes: &[u8]) -> (Vec<Field>, bool) {
    let mut fields = Vec::new();
    let mut polymorphic = false;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != "field_declaration" {
            continue;
        }
        let decl = match child.child_by_field_name("declarator") {
            Some(d) => d,
            None => continue, // e.g. an anonymous bitfield; skip
        };
        // Member function declarations are also `field_declaration`s; they are
        // not data fields. Detect `virtual` for the polymorphism note, then skip.
        if has_function_declarator(decl) {
            if squeeze(text(child, bytes)).starts_with("virtual") {
                polymorphic = true;
            }
            continue;
        }
        let Some(name) = innermost_name(decl, bytes) else {
            continue;
        };
        let ty = child
            .child_by_field_name("type")
            .map(|t| squeeze(text(t, bytes)))
            .unwrap_or_default();
        let non_pod = cpp_non_pod_reason(&ty);
        fields.push(Field { ty, name, non_pod });
    }
    (fields, polymorphic)
}

/// True when a declarator wraps a `function_declarator` (a member function).
fn has_function_declarator(node: Node) -> bool {
    if node.kind() == "function_declarator" {
        return true;
    }
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(has_function_declarator);
    found
}

/// Descend pointer/reference/array/parenthesized declarators to the field name.
fn innermost_name(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "field_identifier" | "identifier" => Some(text(node, bytes).to_string()),
        _ => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return innermost_name(inner, bytes);
            }
            let mut cursor = node.walk();
            let name = node
                .children(&mut cursor)
                .find_map(|c| innermost_name(c, bytes));
            name
        }
    }
}

fn cpp_non_pod_reason(ty: &str) -> Option<String> {
    const MARKERS: [&str; 12] = [
        "std::string",
        "std::vector",
        "std::unordered_map",
        "std::map",
        "std::unordered_set",
        "std::set",
        "std::unique_ptr",
        "std::shared_ptr",
        "std::function",
        "std::list",
        "std::deque",
        "std::optional<std::string",
    ];
    MARKERS
        .iter()
        .find(|m| ty.contains(*m))
        .map(|m| m.to_string())
}

// --- Rust -----------------------------------------------------------------

fn find_rust_struct(node: Node, bytes: &[u8], ty: &str) -> Option<Layout> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "struct_item" {
            if let Some(name) = child.child_by_field_name("name") {
                if text(name, bytes) == ty {
                    if let Some(body) = child.child_by_field_name("body") {
                        if body.kind() == "field_declaration_list" {
                            return Some(Layout {
                                rel: String::new(),
                                line: child.start_position().row + 1,
                                kind: "struct".to_string(),
                                fields: rust_fields(body, bytes),
                                polymorphic: false,
                            });
                        }
                    }
                }
            }
        }
        if let Some(found) = find_rust_struct(child, bytes, ty) {
            return Some(found);
        }
    }
    None
}

fn rust_fields(list: Node, bytes: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if child.kind() != "field_declaration" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .map(|n| text(n, bytes).to_string())
            .unwrap_or_default();
        let ty = child
            .child_by_field_name("type")
            .map(|t| squeeze(text(t, bytes)))
            .unwrap_or_default();
        let non_pod = rust_non_pod_reason(&ty);
        out.push(Field { ty, name, non_pod });
    }
    out
}

fn rust_non_pod_reason(ty: &str) -> Option<String> {
    const MARKERS: [&str; 7] = [
        "String",
        "Vec<",
        "Box<",
        "Rc<",
        "Arc<",
        "HashMap<",
        "BTreeMap<",
    ];
    MARKERS
        .iter()
        .find(|m| ty.contains(*m))
        .map(|m| m.trim_end_matches('<').to_string())
}

// --- Rendering ------------------------------------------------------------

fn render(ty: &str, l: &Layout, budget: usize) -> String {
    let mut out = format!(
        "type_layout - {ty}  ({} at {}:{})\n\nfields ({}):\n",
        l.kind,
        l.rel,
        l.line,
        l.fields.len()
    );
    if l.fields.is_empty() {
        out.push_str("  (none)\n");
    }
    let mut used = out.len() / 4;
    let mut shown = 0usize;
    let mut non_pod = 0usize;
    for f in &l.fields {
        if f.non_pod.is_some() {
            non_pod += 1;
        }
        let row = match &f.non_pod {
            Some(reason) => format!("  {:<30} {:<20} [non-POD: {reason}]\n", f.ty, f.name),
            None => format!("  {:<30} {}\n", f.ty, f.name),
        };
        let lt = row.len() / 4;
        if used + lt > budget && shown > 0 {
            out.push_str(&format!(
                "  … (+{} more; raise token_budget)\n",
                l.fields.len() - shown
            ));
            break;
        }
        used += lt;
        shown += 1;
        out.push_str(&row);
    }

    out.push_str("\nnotes:\n");
    if l.polymorphic {
        out.push_str("  - has virtual function(s): polymorphic (vptr) - not POD\n");
    }
    if non_pod > 0 {
        out.push_str(&format!(
            "  - {non_pod} field(s) look non-POD (heuristic: std/container/smart-ptr types)\n"
        ));
    }
    if non_pod == 0 && !l.polymorphic {
        out.push_str("  - all fields look POD-friendly\n");
    }
    out
}

// --- shared text helpers (local copies; repo_map's are private) -----------

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// Collapse runs of whitespace to single spaces and trim, so a multi-line type
/// (e.g. a template spanning lines) renders on one row.
fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn cpp_pod_struct_lists_fields_and_flags_pod() {
        let dir = std::env::temp_dir().join(format!("cbtest_layout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/widget.hpp",
            "struct Widget { int id; float value; };\n",
        );
        let out = build(
            &dir,
            &serde_json::json!({ "type": "Widget", "paths": ["src"] }),
        )
        .unwrap();
        assert!(out.contains("id"), "{out}");
        assert!(out.contains("value"), "{out}");
        assert!(out.contains("POD-friendly"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cpp_flags_non_pod_members() {
        let dir = std::env::temp_dir().join(format!("cbtest_layout2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/bag.hpp",
            "#include <string>\n#include <vector>\nstruct Bag { int n; std::string name; std::vector<int> items; };\n",
        );
        let out = build(&dir, &serde_json::json!({ "type": "Bag" })).unwrap();
        assert!(out.contains("non-POD"), "{out}");
        assert!(
            out.contains("std::string") || out.contains("std::vector"),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_struct_fields_and_non_pod() {
        let dir = std::env::temp_dir().join(format!("cbtest_layout3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            "src/lib.rs",
            "pub struct Cfg { pub id: u32, pub name: String }\n",
        );
        let out = build(&dir, &serde_json::json!({ "type": "Cfg" })).unwrap();
        assert!(out.contains("id"), "{out}");
        assert!(out.contains("name"), "{out}");
        assert!(out.contains("non-POD"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_type_is_graceful() {
        let dir = std::env::temp_dir().join(format!("cbtest_layout4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "src/a.hpp", "struct Other { int x; };\n");
        let out = build(&dir, &serde_json::json!({ "type": "Nope" })).unwrap();
        assert!(out.contains("no struct/class/union"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
