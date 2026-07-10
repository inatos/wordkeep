//! repo_map - a token-budgeted structural overview of a source tree.
//!
//! Uses tree-sitter for exact extraction of top-level namespaces, record types,
//! enums, and function signatures (free + member), returned in source order,
//! across C/C++, GLSL, Rust, Python, and TypeScript/TSX/Svelte; Daslang via a
//! line scanner (or its vendored grammar under `--features daslang`).
//! Token-budgeted so an agent can see the shape of a directory without reading
//! whole files. The per-file extractor is shared with `outline` (single file,
//! with line numbers).

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Node, Parser};

use crate::cache::{self, DiskMap};
use crate::lang::Lang;
use crate::{stats, walk};

const PER_FILE_CAP: usize = 40;

/// Symbol cache keyed by absolute path; the stored mtime invalidates the entry
/// when the file changes. Backed by a JSON file under the shared cache dir, so it
/// is warm in-process *and* survives a cold spawn over a large tree.
static SYMBOL_CACHE: OnceLock<Mutex<DiskMap>> = OnceLock::new();

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let paths = crate::config::paths_from_args(root, args)?;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(8000) as usize;
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::to_lowercase);

    // The grammar is selected per file inside `extract`, so the shared parser
    // starts language-less and is retargeted as the walk crosses languages.
    let mut parser = Parser::new();

    let mut body = String::new();
    let mut used_tokens = 0usize;
    let mut files = 0usize;
    let mut skipped_files = 0usize;
    let mut truncated = false;
    let mut scanned_bytes = 0u64;

    let prune = walk::prune_set(root);
    for p in &paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(pat) = &pattern {
                if !rel.to_lowercase().contains(pat) {
                    continue;
                }
            }
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            let symbols = cached_symbols(&mut parser, lang, path, cache::mtime_ns(&meta));
            if symbols.is_empty() {
                continue;
            }

            let mut block = format!("\n{rel}\n");
            for (i, s) in symbols.iter().enumerate() {
                if i == PER_FILE_CAP {
                    block.push_str(&format!("  … (+{} more)\n", symbols.len() - PER_FILE_CAP));
                    break;
                }
                block.push_str(&format!("  {s}\n"));
            }

            let block_tokens = block.len() / 4; // rough chars-per-token estimate
            if used_tokens + block_tokens > budget && files > 0 {
                truncated = true;
                skipped_files += 1;
                continue;
            }
            used_tokens += block_tokens;
            files += 1;
            body.push_str(&block);
        }
    }

    // Persist any newly extracted symbols so the next cold spawn is warm too.
    if let Some(c) = SYMBOL_CACHE.get() {
        if let Ok(mut store) = c.lock() {
            store.save();
        }
    }

    if body.is_empty() {
        stats::record("repo_map", scanned_bytes / 4, 0);
        return Ok(format!("repo_map: no source symbols found under {paths:?}"));
    }
    let note = if truncated {
        format!(" (truncated by token_budget; +{skipped_files} file(s) omitted - narrow paths or raise token_budget)")
    } else {
        String::new()
    };
    let out = format!(
        "repo_map - {files} file(s), ~{used_tokens} tokens{note}\nroots: {paths:?}\n{body}"
    );
    stats::record("repo_map", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// Return a file's symbols, reusing a cached parse when the on-disk mtime is
/// unchanged. Falls back to a fresh read+`extract` on cache miss or stat failure.
fn cached_symbols(parser: &mut Parser, lang: Lang, path: &Path, mtime_ns: u64) -> Vec<String> {
    let key = path.to_string_lossy();
    let cell = SYMBOL_CACHE.get_or_init(|| Mutex::new(DiskMap::load("repo_map-symbols.json")));
    if let Ok(store) = cell.lock() {
        if let Some(v) = store.get(key.as_ref(), mtime_ns) {
            return v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let syms = extract(parser, lang, &src);
    if let Ok(mut store) = cell.lock() {
        let payload = Value::Array(syms.iter().cloned().map(Value::String).collect());
        store.put(key.as_ref(), mtime_ns, payload);
    }
    syms
}

/// Exact top-level structure per language with each symbol's start byte offset,
/// source-ordered and de-duplicated (first occurrence wins). tree-sitter drives
/// C/C++, GLSL, Rust, Python, and TypeScript/TSX/Svelte; a small line scanner
/// handles Daslang without the grammar. Svelte is reduced to its `<script>`
/// blocks first (newlines preserved, so byte offsets still map to the file).
/// `repo_map` drops the offset; `outline` turns it into a line number. The
/// shared parser is retargeted to the file's grammar on each call.
pub fn symbols_located(parser: &mut Parser, lang: Lang, src: &str) -> Vec<(usize, String)> {
    let prepared = lang.preprocess(src);
    let mut found: Vec<(usize, String)> = match lang.ts_language() {
        Some(ts) => {
            if parser.set_language(&ts).is_err() {
                return Vec::new();
            }
            let Some(tree) = parser.parse(prepared.as_ref(), None) else {
                return Vec::new();
            };
            let bytes = prepared.as_bytes();
            let mut f = Vec::new();
            match lang {
                Lang::Rust => collect_rust(tree.root_node(), bytes, &mut f),
                Lang::Python => collect_python(tree.root_node(), bytes, &mut f),
                Lang::Ts | Lang::Tsx | Lang::Svelte => collect_ts(tree.root_node(), bytes, &mut f),
                Lang::CSharp => collect_csharp(tree.root_node(), bytes, &mut f),
                #[cfg(feature = "daslang")]
                Lang::Daslang => collect_das(tree.root_node(), bytes, &mut f),
                // C/C++ and GLSL (a tree-sitter-c fork) share the C node kinds.
                _ => collect_cpp(tree.root_node(), bytes, &mut f),
            }
            f
        }
        // Only Daslang reaches here, and only without the `daslang` feature;
        // every other language resolves to a tree-sitter grammar above.
        None => {
            #[cfg(not(feature = "daslang"))]
            {
                das_symbols(prepared.as_ref())
            }
            #[cfg(feature = "daslang")]
            {
                Vec::new()
            }
        }
    };
    found.sort_by_key(|(b, _)| *b);

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (b, s) in found {
        if seen.insert(s.clone()) {
            out.push((b, s));
        }
    }
    out
}

/// Structural symbols for `repo_map` - [`symbols_located`] with the byte offset
/// dropped.
fn extract(parser: &mut Parser, lang: Lang, src: &str) -> Vec<String> {
    symbols_located(parser, lang, src)
        .into_iter()
        .map(|(_, s)| s)
        .collect()
}

/// Recurse a C/C++ tree, emitting `(start_byte, symbol)` for the constructs we
/// map. Descends into namespace/record bodies so member functions are captured.
fn collect_cpp(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_definition" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("namespace {}", text(name, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_cpp(b, src, out);
                }
            }
            "struct_specifier" | "class_specifier" | "union_specifier" => {
                let kw = match child.kind() {
                    "struct_specifier" => "struct",
                    "class_specifier" => "class",
                    _ => "union",
                };
                // Require a body so forward declarations are skipped.
                if let (Some(name), Some(b)) = (
                    child.child_by_field_name("name"),
                    child.child_by_field_name("body"),
                ) {
                    out.push((child.start_byte(), format!("{kw} {}", text(name, src))));
                    collect_cpp(b, src, out);
                }
            }
            "enum_specifier" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("enum {}", text(name, src))));
                }
            }
            "function_definition" => {
                if let Some(sig) = fn_signature(child, src) {
                    out.push((child.start_byte(), sig));
                }
            }
            "declaration" | "field_declaration" => {
                if let Some(d) = child.child_by_field_name("declarator") {
                    if d.kind() == "function_declarator" {
                        let raw = text(child, src).trim().trim_end_matches(';').trim();
                        out.push((child.start_byte(), squeeze(raw)));
                    }
                }
            }
            "template_declaration" | "linkage_specification" => collect_cpp(child, src, out),
            _ => {}
        }
    }
}

/// Recurse a Rust tree: modules, records, traits, impls, and fn signatures.
/// Descends into `mod`/`trait`/`impl` bodies so methods are captured.
fn collect_rust(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("mod {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "struct_item" | "enum_item" | "union_item" => {
                let kw = match child.kind() {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    _ => "union",
                };
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("{kw} {}", text(n, src))));
                }
            }
            "trait_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("trait {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "impl_item" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_rust(b, src, out);
                }
            }
            "function_item" | "function_signature_item" => {
                out.push((child.start_byte(), header(child, src)));
            }
            "type_item" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("type {}", text(n, src))));
                }
            }
            "macro_definition" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("macro_rules! {}", text(n, src))));
                }
            }
            _ => {}
        }
    }
}

/// Recurse a Python tree: classes and `def`s (sync + async). Descends into class
/// bodies so methods are captured; unwraps decorators.
fn collect_python(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_python(b, src, out);
                }
            }
            "function_definition" => {
                out.push((child.start_byte(), header(child, src)));
            }
            "decorated_definition" => collect_python(child, src, out),
            _ => {}
        }
    }
}

/// Recurse a TypeScript/TSX/Svelte tree: classes (+ methods), interfaces, type
/// aliases, enums, function declarations, and top-level `const f = () => {}`
/// bindings. Descends class bodies for methods and unwraps `export`.
fn collect_ts(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("class {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_ts(b, src, out);
                }
            }
            "interface_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("interface {}", text(n, src))));
                }
            }
            "type_alias_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("type {}", text(n, src))));
                }
            }
            "enum_declaration" => {
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("enum {}", text(n, src))));
                }
            }
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                out.push((child.start_byte(), header(child, src)));
            }
            // `export` wraps a declaration; recurse so the inner one is captured.
            "export_statement" => collect_ts(child, src, out),
            // Top-level `const f = () => {}` / `let g = function () {}`.
            "lexical_declaration" | "variable_declaration" => {
                let mut c2 = child.walk();
                for d in child.children(&mut c2) {
                    if d.kind() == "variable_declarator" {
                        let is_fn = d
                            .child_by_field_name("value")
                            .map(|v| {
                                matches!(
                                    v.kind(),
                                    "arrow_function" | "function" | "function_expression"
                                )
                            })
                            .unwrap_or(false);
                        if is_fn {
                            if let Some(n) = d.child_by_field_name("name") {
                                out.push((d.start_byte(), format!("const {}()", text(n, src))));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Recurse a C# tree: namespaces, records, classes, and member declarations.
fn collect_csharp(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("namespace {}", text(name, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "record_struct_declaration" => {
                let kw = match child.kind() {
                    "class_declaration" => "class",
                    "struct_declaration" => "struct",
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    _ => "record",
                };
                if let Some(n) = child.child_by_field_name("name") {
                    out.push((child.start_byte(), format!("{kw} {}", text(n, src))));
                }
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "property_declaration"
            | "indexer_declaration"
            | "delegate_declaration" => {
                out.push((child.start_byte(), header(child, src)));
                if let Some(b) = child.child_by_field_name("body") {
                    collect_csharp(b, src, out);
                }
            }
            _ => {}
        }
    }
}

/// A declaration header: text from the node start to its `body` (or end), with
/// whitespace squeezed and a trailing `;` trimmed. Works for Rust fn/impl and
/// Python def/class (keeps the trailing `:`).
fn header(node: Node, src: &[u8]) -> String {
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let s = std::str::from_utf8(src.get(node.start_byte()..end).unwrap_or(b"")).unwrap_or("");
    squeeze(s.trim().trim_end_matches(';').trim())
}

/// Daslang (`.das`) extraction via the vendored tree-sitter grammar (enabled by
/// the `daslang` feature). Recurses the whole tree, emitting a label for each
/// outline node kind defined in the grammar's `outline_rules.yml`.
#[cfg(feature = "daslang")]
fn collect_das(node: Node, src: &[u8], out: &mut Vec<(usize, String)>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(label) = das_label(child, src) {
            out.push((child.start_byte(), label));
        }
        collect_das(child, src, out);
    }
}

/// Label for a Daslang outline node, or `None` if it is not one we map.
/// `function_declaration` keeps its full signature (up to the body); the others
/// are `keyword name` from the node's `name` field.
#[cfg(feature = "daslang")]
fn das_label(node: Node, src: &[u8]) -> Option<String> {
    let name = || {
        node.child_by_field_name("name")
            .map(|n| text(n, src).to_string())
    };
    match node.kind() {
        "function_declaration" => Some(header(node, src)),
        "structure_declaration" => {
            let kw = node
                .child_by_field_name("kind")
                .map(|k| text(k, src))
                .unwrap_or("struct");
            name().map(|n| format!("{kw} {n}"))
        }
        "enum_declaration" => name().map(|n| format!("enum {n}")),
        "variant_alias_declaration" => name().map(|n| format!("variant {n}")),
        "typedef_statement" => name().map(|n| format!("typedef {n}")),
        "bitfield_alias_declaration" => name().map(|n| format!("bitfield {n}")),
        _ => None,
    }
}

/// Daslang fallback used when the `daslang` feature is off: scan top-level
/// declarations by leading keyword - `def` (whole signature line),
/// `struct`/`class`/`enum`/`variant` (keyword + name). Indentation-based bodies
/// are ignored. With the feature on, `collect_das` uses the real grammar.
#[cfg(not(feature = "daslang"))]
fn das_symbols(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for line in src.lines() {
        let t = line.trim_start();
        if after_kw(t, "def").is_some() {
            out.push((off, squeeze(t)));
        } else if let Some(r) = after_kw(t, "struct") {
            out.push((off, format!("struct {}", ident(r))));
        } else if let Some(r) = after_kw(t, "class") {
            out.push((off, format!("class {}", ident(r))));
        } else if let Some(r) = after_kw(t, "enum") {
            out.push((off, format!("enum {}", ident(r))));
        } else if let Some(r) = after_kw(t, "variant") {
            out.push((off, format!("variant {}", ident(r))));
        }
        off += line.len() + 1; // +1 for the newline `lines()` stripped
    }
    out
}

/// The remainder after a leading keyword that is followed by whitespace, else
/// `None` (so `definitely` does not match the keyword `def`).
#[cfg(not(feature = "daslang"))]
fn after_kw<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let r = line.strip_prefix(kw)?;
    if r.starts_with([' ', '\t']) {
        Some(r.trim_start())
    } else {
        None
    }
}

/// The leading identifier (`[A-Za-z0-9_]+`) of a string.
#[cfg(not(feature = "daslang"))]
fn ident(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// Signature of a function definition: text from its start up to the body `{`.
fn fn_signature(node: Node, src: &[u8]) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let s = std::str::from_utf8(src.get(node.start_byte()..end)?).ok()?;
    Some(squeeze(s.trim()))
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// Collapse interior whitespace to single spaces and cap the length.
fn squeeze(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 110 {
        one
    } else {
        let t: String = one.chars().take(109).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> Parser {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        p
    }

    #[test]
    fn extract_finds_namespace_record_enum_and_fns() {
        let src = r#"
namespace game {
struct Transform { float x; float y; };
enum class State { Idle, Run };
void update(Transform& t);
int Transform_area(const Transform& t) { return 0; }
}
"#;
        let syms = extract(&mut parser(), Lang::Cpp, src);
        assert!(syms.iter().any(|s| s == "namespace game"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Transform"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum State"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("update(")), "{syms:?}");
        assert!(
            syms.iter().any(|s| s.contains("Transform_area(")),
            "{syms:?}"
        );
    }

    #[test]
    fn extract_skips_forward_declarations() {
        // No body → not a real definition, should be omitted.
        let syms = extract(&mut parser(), Lang::Cpp, "struct OnlyForward;\n");
        assert!(!syms.iter().any(|s| s == "struct OnlyForward"), "{syms:?}");
    }

    #[test]
    fn extract_is_source_ordered_and_deduped() {
        let src = "struct A {};\nstruct B {};\nstruct A {};\n";
        let syms = extract(&mut parser(), Lang::Cpp, src);
        let a = syms.iter().position(|s| s == "struct A");
        let b = syms.iter().position(|s| s == "struct B");
        assert!(a.is_some() && b.is_some());
        assert!(a.unwrap() < b.unwrap(), "source order: {syms:?}");
        assert_eq!(syms.iter().filter(|s| *s == "struct A").count(), 1);
    }

    #[test]
    fn extract_rust_items_and_methods() {
        let src = "pub mod net {\n    pub struct Peer { id: u32 }\n    pub enum State { Up, Down }\n    impl Peer {\n        pub fn ping(&self) -> bool { true }\n    }\n    pub fn connect(addr: &str) -> Peer { todo!() }\n}\n";
        let syms = extract(&mut parser(), Lang::Rust, src);
        assert!(syms.iter().any(|s| s == "mod net"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Peer"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum State"), "{syms:?}");
        assert!(syms.iter().any(|s| s.starts_with("impl Peer")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("fn ping")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("fn connect")), "{syms:?}");
    }

    #[test]
    fn extract_python_classes_and_defs() {
        let src = "import os\n\nclass Atlas:\n    def bake(self, path):\n        return path\n\n@cache\ndef build(name):\n    return name\n";
        let syms = extract(&mut parser(), Lang::Python, src);
        assert!(
            syms.iter().any(|s| s.starts_with("class Atlas")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s.contains("def bake")), "{syms:?}");
        // decorated def is unwrapped and captured
        assert!(syms.iter().any(|s| s.contains("def build")), "{syms:?}");
    }

    #[test]
    fn extract_typescript_items_and_methods() {
        let src = "export class Atlas {\n  bake(path: string): string { return path; }\n}\ninterface Shape { area(): number }\ntype Id = number;\nenum Color { Red, Green }\nexport function build(name: string): string { return name; }\nconst make = (n: number) => n + 1;\n";
        let syms = extract(&mut parser(), Lang::Ts, src);
        assert!(syms.iter().any(|s| s == "class Atlas"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("bake")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "interface Shape"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "type Id"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Color"), "{syms:?}");
        assert!(
            syms.iter().any(|s| s.contains("function build")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s.contains("const make")), "{syms:?}");
    }

    #[test]
    fn extract_csharp_classes_and_methods() {
        let src = "namespace App.Exporter {\n  public class Plugin {\n    public void Awake() { Logger.LogInfo(\"hi\"); }\n  }\n  public static class BundleWriter {\n    public static void Write(string dir) {}\n  }\n}\n";
        let syms = extract(&mut parser(), Lang::CSharp, src);
        assert!(
            syms.iter().any(|s| s.contains("namespace App.Exporter")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s == "class Plugin"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("Awake")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "class BundleWriter"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("Write")), "{syms:?}");
    }

    #[test]
    fn extract_svelte_reads_script_only() {
        let src = "<script lang=\"ts\">\nexport function onMount() {}\nconst label = \"hi\";\n</script>\n\n<h1>{label}</h1>\n";
        let syms = extract(&mut parser(), Lang::Svelte, src);
        assert!(
            syms.iter().any(|s| s.contains("function onMount")),
            "{syms:?}"
        );
        // markup is not parsed as code
        assert!(!syms.iter().any(|s| s.contains("h1")), "{syms:?}");
    }

    // Default build: the line scanner handles indentation-style Daslang
    // (def name + indented bodies, no braces).
    #[cfg(not(feature = "daslang"))]
    #[test]
    fn extract_daslang_indentation_via_scanner() {
        let src = "require app_core\n\n[export]\ndef tick_frozen(t : float; dt : float) : float\n    return t - dt\n\nstruct Spark\n    energy : float\n\nenum Element\n    Fire\n";
        let syms = extract(&mut parser(), Lang::Daslang, src);
        assert!(
            syms.iter().any(|s| s.contains("def tick_frozen(")),
            "{syms:?}"
        );
        assert!(syms.iter().any(|s| s == "struct Spark"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Element"), "{syms:?}");
        // `require` must not be mistaken for a definition
        assert!(!syms.iter().any(|s| s.contains("require")), "{syms:?}");
    }

    // `daslang` feature: the vendored grammar parses daScript's gen2 brace syntax
    // (the indentation dialect is not supported upstream - see vendor SOURCE.txt).
    #[cfg(feature = "daslang")]
    #[test]
    fn extract_daslang_brace_via_grammar() {
        let src = "require app_core\n\ndef add(a : int, b : int) : int {\n    return a + b\n}\n\nstruct Spark {\n    energy : float\n}\n\nenum Element {\n    Fire\n}\n";
        let syms = extract(&mut parser(), Lang::Daslang, src);
        assert!(syms.iter().any(|s| s.contains("def add(")), "{syms:?}");
        assert!(syms.iter().any(|s| s == "struct Spark"), "{syms:?}");
        assert!(syms.iter().any(|s| s == "enum Element"), "{syms:?}");
        // `require` must not be mistaken for a definition
        assert!(!syms.iter().any(|s| s.contains("require")), "{syms:?}");
    }

    #[test]
    fn extract_glsl_struct_and_functions() {
        let src = "#version 330 core\nstruct Light { vec3 pos; };\nvec4 shade(vec3 n) {\n    return vec4(n, 1.0);\n}\nvoid main() {\n    if (true) { discard; }\n}\n";
        let syms = extract(&mut parser(), Lang::Glsl, src);
        assert!(syms.iter().any(|s| s == "struct Light"), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("shade(")), "{syms:?}");
        assert!(syms.iter().any(|s| s.contains("main(")), "{syms:?}");
        // control-flow keywords are not functions
        assert!(!syms.iter().any(|s| s.contains("if (")), "{syms:?}");
    }

    #[test]
    fn cached_symbols_reuses_until_mtime_changes() {
        let dir = std::env::temp_dir().join(format!("cbtest_repomap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.hpp");
        std::fs::write(&f, "struct First {};\n").unwrap();
        let mut p = parser();
        let m1 = std::fs::metadata(&f).unwrap();
        let t1 = cache::mtime_ns(&m1);
        let s1 = cached_symbols(&mut p, Lang::Cpp, &f, t1);
        assert!(s1.iter().any(|s| s == "struct First"));
        // Same mtime → cached clone, even though disk content "changed" logically.
        let again = cached_symbols(&mut p, Lang::Cpp, &f, t1);
        assert_eq!(s1, again);
        // New content + new mtime → re-extract.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "struct Second {};\n").unwrap();
        let m2 = std::fs::metadata(&f).unwrap();
        let t2 = cache::mtime_ns(&m2);
        let s2 = cached_symbols(&mut p, Lang::Cpp, &f, t2);
        assert!(s2.iter().any(|s| s == "struct Second"), "{s2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn squeeze_collapses_and_caps() {
        assert_eq!(squeeze("a   b\n\tc"), "a b c");
        let long = "x".repeat(200);
        let out = squeeze(&long);
        assert!(out.chars().count() <= 110);
        assert!(out.ends_with('…'));
    }
}
