//! symbol_def - locate a symbol's definition and distill what the
//! navigation tools need from it: signature, leading doc comment, body, line
//! span, and a coarse kind. Shared by `doc_comment`, `symbol_context`, and
//! `diff_map` so the per-language "which node is a definition" knowledge lives
//! in one place.
//!
//! Parsing goes through the [`crate::incremental`] tree cache, so repeated
//! lookups in a session re-use warm trees. Matching mirrors `call_graph`: a
//! query lines up with the last `::` segment of a qualified name, so `Bar`
//! finds `Foo::Bar` and vice versa.

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

use crate::lang::Lang;
use crate::{cache, incremental, walk};

/// Coarse classification of a definition, enough to decide downstream framing
/// (e.g. `symbol_context` only fetches a `type_layout` for a record).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefKind {
    Function,
    Record,
    Other,
}

impl DefKind {
    pub fn label(self) -> &'static str {
        match self {
            DefKind::Function => "function",
            DefKind::Record => "record",
            DefKind::Other => "symbol",
        }
    }
}

/// What a located definition yields. `body` is only populated when the caller
/// asks for it (`want_body`), since it can be large.
pub struct DefInfo {
    pub rel: String,
    pub line: usize,     // 1-based start line
    pub end_line: usize, // 1-based end line
    pub kind: DefKind,
    pub name: String, // the matched declared name (possibly qualified)
    pub signature: String,
    pub doc: Vec<String>,
    pub body: String,
}

/// A function/method definition's line span, for `diff_map` to intersect with
/// changed lines. `signature` lets `diff_map` tell an ABI-breaking signature
/// change from a body-only edit by comparing it across git revisions.
pub struct Span {
    pub name: String,
    pub start: usize, // 1-based
    pub end: usize,   // 1-based
    pub signature: String,
}

/// A brief on one definition for `undocumented`: its name, kind, 1-based line,
/// signature, and whether a leading doc comment precedes it. No body, so it
/// stays cheap to enumerate across a whole tree.
pub struct DefBrief {
    pub name: String,
    pub kind: DefKind,
    pub line: usize,
    pub signature: String,
    pub has_doc: bool,
    /// Whether the definition looks publicly exported (language-specific).
    pub exported: bool,
}

/// Languages with a tree-sitter grammar we can locate definitions in. Daslang is
/// excluded (no default grammar); `repo_map` maps its declarations instead.
fn supported(lang: Lang) -> bool {
    matches!(
        lang,
        Lang::Cpp
            | Lang::Glsl
            | Lang::Rust
            | Lang::Python
            | Lang::Ts
            | Lang::Tsx
            | Lang::Svelte
            | Lang::CSharp
    )
}

/// Walk `paths` in sorted order and return the first definition whose name
/// matches `symbol`, plus the number of bytes scanned (for the savings
/// counter). Files are parsed through the incremental cache.
pub fn locate(
    root: &Path,
    symbol: &str,
    paths: &[String],
    want_body: bool,
) -> (Option<DefInfo>, u64) {
    let prune = walk::prune_set(root);
    let mut cands: Vec<(PathBuf, Lang, u64)> = Vec::new();
    for p in paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            if !supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            cands.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0));

    let mut parser = Parser::new();
    let mut scanned = 0u64;
    for (path, lang, mt) in &cands {
        let Some((src, tree)) = incremental::parse(&mut parser, *lang, path, *mt) else {
            continue;
        };
        scanned += src.len() as u64;
        // Cheap prefilter: the name must literally appear to be defined here.
        if !src.contains(symbol) {
            continue;
        }
        let bytes = src.as_bytes();
        if let Some((node, kind, name)) = find_def(tree.root_node(), *lang, bytes, symbol) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let info = DefInfo {
                rel,
                line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
                kind,
                name,
                signature: signature_of(node, bytes),
                doc: leading_doc(node, *lang, bytes),
                body: if want_body {
                    body_text(node, bytes)
                } else {
                    String::new()
                },
            };
            return (Some(info), scanned);
        }
    }
    (None, scanned)
}

/// All function/method definition spans in one already-known file, for
/// `diff_map`. Parsed through the incremental cache.
pub fn function_spans(parser: &mut Parser, lang: Lang, path: &Path, mtime: u64) -> Vec<Span> {
    if !supported(lang) {
        return Vec::new();
    }
    let Some((src, tree)) = incremental::parse(parser, lang, path, mtime) else {
        return Vec::new();
    };
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    collect_spans(tree.root_node(), lang, bytes, &mut out);
    out
}

fn collect_spans(node: Node, lang: Lang, bytes: &[u8], out: &mut Vec<Span>) {
    if let Some((name, kind)) = def_name(node, lang, bytes) {
        if kind == DefKind::Function {
            out.push(Span {
                name,
                start: node.start_position().row + 1,
                end: node.end_position().row + 1,
                signature: signature_of(node, bytes),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_spans(child, lang, bytes, out);
    }
}

/// Function/method spans parsed straight from a source string (no file, no
/// cache), so `diff_map` can compare a definition's signature against the blob
/// at a git revision. Uses a fresh parser; empty for unsupported languages.
pub fn function_spans_from_str(lang: Lang, src: &str) -> Vec<Span> {
    if !supported(lang) {
        return Vec::new();
    }
    let Some(ts) = lang.ts_language() else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts).is_err() {
        return Vec::new();
    }
    let prepared = lang.preprocess(src);
    let Some(tree) = parser.parse(prepared.as_ref(), None) else {
        return Vec::new();
    };
    let bytes = prepared.as_bytes();
    let mut out = Vec::new();
    collect_spans(tree.root_node(), lang, bytes, &mut out);
    out
}

/// Every recognized definition in one file (no bodies), each flagged with
/// whether it carries a leading doc comment, for `undocumented`. Parsed through
/// the incremental cache so a re-touched file stays warm.
pub fn definitions(parser: &mut Parser, lang: Lang, path: &Path, mtime: u64) -> Vec<DefBrief> {
    if !supported(lang) {
        return Vec::new();
    }
    let Some((src, tree)) = incremental::parse(parser, lang, path, mtime) else {
        return Vec::new();
    };
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    collect_defs(tree.root_node(), lang, bytes, &mut out);
    out
}

fn collect_defs(node: Node, lang: Lang, bytes: &[u8], out: &mut Vec<DefBrief>) {
    if let Some((name, kind)) = def_name(node, lang, bytes) {
        out.push(DefBrief {
            name,
            kind,
            line: node.start_position().row + 1,
            signature: signature_of(node, bytes),
            has_doc: !leading_doc(node, lang, bytes).is_empty(),
            exported: is_exported(node, lang, bytes),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_defs(child, lang, bytes, out);
    }
}

/// Depth-first, source-order search for the first definition node whose name
/// matches `symbol`. Returns the node, its kind, and the matched name.
fn find_def<'t>(
    node: Node<'t>,
    lang: Lang,
    bytes: &[u8],
    symbol: &str,
) -> Option<(Node<'t>, DefKind, String)> {
    if let Some((name, kind)) = def_name(node, lang, bytes) {
        if name_matches(&name, symbol) {
            return Some((node, kind, name));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_def(child, lang, bytes, symbol) {
            return Some(found);
        }
    }
    None
}

/// If `node` is a definition we recognize, its declared name and kind.
fn def_name(node: Node, lang: Lang, bytes: &[u8]) -> Option<(String, DefKind)> {
    let named = |field: &str, kind: DefKind| {
        node.child_by_field_name(field)
            .map(|n| (text(n, bytes).to_string(), kind))
    };
    match lang {
        Lang::Cpp | Lang::Glsl => match node.kind() {
            "function_definition" => cpp_function_name(node, bytes).map(|n| (n, DefKind::Function)),
            "struct_specifier" | "class_specifier" | "union_specifier" => {
                // Require a body so forward declarations are skipped.
                node.child_by_field_name("body")?;
                named("name", DefKind::Record)
            }
            "enum_specifier" => named("name", DefKind::Record),
            _ => None,
        },
        Lang::Rust => match node.kind() {
            "function_item" | "function_signature_item" => named("name", DefKind::Function),
            "struct_item" | "enum_item" | "union_item" => named("name", DefKind::Record),
            "trait_item" | "type_item" => named("name", DefKind::Other),
            _ => None,
        },
        Lang::Python => match node.kind() {
            "function_definition" => named("name", DefKind::Function),
            "class_definition" => named("name", DefKind::Record),
            _ => None,
        },
        Lang::Ts | Lang::Tsx | Lang::Svelte => match node.kind() {
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                named("name", DefKind::Function)
            }
            "class_declaration" | "abstract_class_declaration" => named("name", DefKind::Record),
            "interface_declaration" | "enum_declaration" => named("name", DefKind::Record),
            "type_alias_declaration" => named("name", DefKind::Other),
            // `const f = () => {}` / `let g = function () {}`.
            "variable_declarator" => {
                let is_fn = node
                    .child_by_field_name("value")
                    .map(|v| {
                        matches!(
                            v.kind(),
                            "arrow_function" | "function" | "function_expression"
                        )
                    })
                    .unwrap_or(false);
                is_fn.then(|| named("name", DefKind::Function)).flatten()
            }
            _ => None,
        },
        Lang::CSharp => match node.kind() {
            "method_declaration" | "constructor_declaration" | "destructor_declaration" => {
                named("name", DefKind::Function)
            }
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "record_struct_declaration" => named("name", DefKind::Record),
            "property_declaration" | "delegate_declaration" => named("name", DefKind::Other),
            _ => None,
        },
        Lang::Daslang => None,
    }
}

/// The declared name of a C/C++ `function_definition`, e.g. `foo` or `Foo::Bar`.
fn cpp_function_name(def: Node, bytes: &[u8]) -> Option<String> {
    let decl = def.child_by_field_name("declarator")?;
    let fdecl = find_function_declarator(decl)?;
    let name = fdecl.child_by_field_name("declarator")?;
    Some(text(name, bytes).to_string())
}

fn find_function_declarator(node: Node) -> Option<Node> {
    if node.kind() == "function_declarator" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(f) = find_function_declarator(child) {
            return Some(f);
        }
    }
    None
}

/// The body block of a definition, for trimming a signature and reading a body.
/// Falls back to a function-valued binding's body (`const f = () => { … }`).
fn body_of(node: Node) -> Option<Node> {
    if let Some(b) = node.child_by_field_name("body") {
        return Some(b);
    }
    node.child_by_field_name("value")
        .and_then(|v| v.child_by_field_name("body"))
}

/// Text from the node's start up to its body (or its end), whitespace-squeezed
/// and capped - the declaration line(s) without the body.
fn signature_of(node: Node, bytes: &[u8]) -> String {
    let end = body_of(node)
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let s = std::str::from_utf8(bytes.get(node.start_byte()..end).unwrap_or(b"")).unwrap_or("");
    squeeze(s.trim().trim_end_matches('{').trim(), 200)
}

/// The full source text of the definition node.
fn body_text(node: Node, bytes: &[u8]) -> String {
    std::str::from_utf8(bytes.get(node.byte_range()).unwrap_or(b""))
        .unwrap_or("")
        .to_string()
}

/// The leading documentation for a definition: a Python docstring, or the run of
/// comment lines immediately above the declaration (climbing past `export`,
/// decorators, templates, and Rust attributes).
fn leading_doc(node: Node, lang: Lang, bytes: &[u8]) -> Vec<String> {
    if lang == Lang::Python {
        let d = python_docstring(node, bytes);
        if !d.is_empty() {
            return d;
        }
    }

    let a = anchor(node, lang);
    let mut blocks: Vec<Node> = Vec::new();
    let mut expect_row = a.start_position().row;
    let mut first = true;
    let mut sib = a.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "comment" | "line_comment" | "block_comment" => {
                let gap = expect_row.saturating_sub(s.end_position().row);
                let ok = if first { gap <= 2 } else { gap <= 1 };
                if !ok {
                    break;
                }
                expect_row = s.start_position().row;
                blocks.push(s);
                first = false;
                sib = s.prev_sibling();
            }
            // Attributes sit between a Rust doc comment and its item; skip them.
            "attribute_item" if lang == Lang::Rust => {
                expect_row = s.start_position().row;
                sib = s.prev_sibling();
            }
            _ => break,
        }
    }
    blocks.reverse();
    let mut out = Vec::new();
    for b in blocks {
        out.extend(clean_comment(text(b, bytes)));
    }
    out
}

/// Whether a definition is publicly visible/exported, per language. C/C++/GLSL
/// returns false (header paths are handled separately by `dead_code`).
fn is_exported(node: Node, lang: Lang, bytes: &[u8]) -> bool {
    match lang {
        Lang::Rust => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "visibility_modifier" {
                    return true;
                }
            }
            false
        }
        Lang::Ts | Lang::Tsx | Lang::Svelte => {
            let mut n = node;
            while let Some(p) = n.parent() {
                if p.kind() == "export_statement" {
                    return true;
                }
                n = p;
            }
            false
        }
        Lang::CSharp => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "modifier"
                    && matches!(
                        text(child, bytes).trim(),
                        "public" | "protected" | "internal"
                    )
                {
                    return true;
                }
            }
            false
        }
        Lang::Python => is_python_toplevel(node),
        Lang::Cpp | Lang::Glsl | Lang::Daslang => false,
    }
}

/// Python defs/classes at module scope are the public surface of a `.py` file.
fn is_python_toplevel(node: Node) -> bool {
    match node.parent().map(|p| p.kind()) {
        Some("module") => true,
        Some("decorated_definition") => node
            .parent()
            .and_then(|p| p.parent())
            .map(|gp| gp.kind() == "module")
            .unwrap_or(false),
        _ => false,
    }
}

/// Climb from a definition to the node that actually carries any preceding
/// comment: `export` wrappers (TS), decorators (Python), templates (C++).
fn anchor<'t>(node: Node<'t>, lang: Lang) -> Node<'t> {
    let mut n = node;
    while let Some(p) = n.parent() {
        let climb = match lang {
            Lang::Ts | Lang::Tsx | Lang::Svelte => p.kind() == "export_statement",
            Lang::Python => p.kind() == "decorated_definition",
            Lang::Cpp | Lang::Glsl => p.kind() == "template_declaration",
            _ => false,
        };
        if climb {
            n = p;
        } else {
            break;
        }
    }
    n
}

/// The first string statement in a Python def/class body, cleaned of its quotes.
fn python_docstring(node: Node, bytes: &[u8]) -> Vec<String> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let Some(stmt) = body.children(&mut cursor).next() else {
        return Vec::new();
    };
    if stmt.kind() != "expression_statement" {
        return Vec::new();
    }
    let mut c2 = stmt.walk();
    for e in stmt.children(&mut c2) {
        if e.kind() == "string" {
            return clean_pydoc(text(e, bytes));
        }
    }
    Vec::new()
}

/// Strip comment markers from a (possibly multi-line) comment node into cleaned
/// lines, dropping lines that are pure markers.
fn clean_comment(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        let t = t
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start_matches("//")
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim_start_matches('*')
            .trim();
        if !t.is_empty() {
            out.push(squeeze(t, 200));
        }
    }
    out
}

/// Strip the surrounding triple/single quotes (and `r`/`b` prefixes) of a Python
/// string literal into cleaned doc lines.
fn clean_pydoc(raw: &str) -> Vec<String> {
    let s = raw.trim();
    let s = s.trim_start_matches(['r', 'R', 'b', 'B', 'f', 'F', 'u', 'U']);
    let s = s
        .trim_start_matches("\"\"\"")
        .trim_end_matches("\"\"\"")
        .trim_start_matches("'''")
        .trim_end_matches("'''")
        .trim_matches('"')
        .trim_matches('\'');
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| squeeze(l, 200))
        .collect()
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// Collapse interior whitespace to single spaces and cap the length.
fn squeeze(s: &str, cap: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= cap {
        one
    } else {
        let t: String = one.chars().take(cap.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Last `::`-separated segment of a (possibly qualified) name.
fn seg(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Match on the full name or its trailing segment.
fn name_matches(a: &str, b: &str) -> bool {
    a == b || seg(a) == seg(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squeeze_caps_length() {
        assert_eq!(squeeze("a  b\n c", 10), "a b c");
        assert!(squeeze(&"x".repeat(50), 10).ends_with('…'));
    }

    #[test]
    fn clean_comment_strips_markers() {
        assert_eq!(clean_comment("/// hello"), vec!["hello".to_string()]);
        assert_eq!(
            clean_comment("/** a\n * b\n */"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn clean_pydoc_strips_triple_quotes() {
        assert_eq!(
            clean_pydoc("\"\"\"Hi there.\"\"\""),
            vec!["Hi there.".to_string()]
        );
    }

    #[test]
    fn name_matches_on_segment() {
        assert!(name_matches("Foo::bar", "bar"));
        assert!(name_matches("bar", "Foo::bar"));
        assert!(!name_matches("baz", "bar"));
    }

    #[test]
    fn spans_from_str_capture_signature() {
        let spans = function_spans_from_str(Lang::Cpp, "int add(int a, int b) { return a + b; }");
        assert_eq!(spans.len(), 1);
        assert_eq!(seg(&spans[0].name), "add");
        assert!(
            spans[0].signature.contains("int add(int a, int b)"),
            "sig was {:?}",
            spans[0].signature
        );
    }

    fn exported_in(src: &str, lang: Lang) -> bool {
        let Some(ts) = lang.ts_language() else {
            return false;
        };
        let mut parser = Parser::new();
        if parser.set_language(&ts).is_err() {
            return false;
        }
        let prepared = lang.preprocess(src);
        let Some(tree) = parser.parse(prepared.as_ref(), None) else {
            return false;
        };
        let bytes = prepared.as_bytes();
        let mut found = false;
        fn walk(node: Node, lang: Lang, bytes: &[u8], found: &mut bool) {
            if def_name(node, lang, bytes).is_some() {
                *found = is_exported(node, lang, bytes);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk(child, lang, bytes, found);
            }
        }
        walk(tree.root_node(), lang, bytes, &mut found);
        found
    }

    #[test]
    fn is_exported_rust_pub_fn() {
        assert!(exported_in("pub fn open() {}", Lang::Rust));
        assert!(!exported_in("fn hidden() {}", Lang::Rust));
    }

    #[test]
    fn is_exported_ts_export_and_private() {
        assert!(exported_in("export function open() {}", Lang::Ts));
        assert!(!exported_in("function hidden() {}", Lang::Ts));
    }

    #[test]
    fn is_exported_csharp_public_method() {
        let src = "namespace N { public class C { public void Open() {} } }";
        assert!(exported_in(src, Lang::CSharp));
    }
}
