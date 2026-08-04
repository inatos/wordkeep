//! call_graph - one hop of callers and callees for a function.
//!
//! Framed around *function boundaries*: given a symbol, it finds the functions
//! that call it (callers) and the functions it calls (callees), each with a
//! file:line site - exactly the blast radius you want before changing a
//! signature. Names match on their last `::` segment, so `Bar`, `Foo::Bar`, and
//! an out-of-line `void Foo::Bar()` all line up. Coverage spans C/C++, GLSL,
//! Rust, Python, and TypeScript/TSX/Svelte; Daslang is left to `repo_map`.
//!
//! Each file is distilled once into symbol-agnostic defs + call edges, cached on
//! disk keyed path+mtime; cold files are parsed in parallel. A query filters the
//! cached graphs, so repeated calls over an unchanged tree skip re-parsing.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Node, Parser};

use crate::cache::{self, DiskMap};
use crate::lang::Lang;
use crate::{stats, walk};

struct Site {
    rel: String,
    line: usize,
    text: String,
}

/// A function definition site, symbol-agnostic (one per `function_definition`).
struct DefSite {
    name: String,
    line: usize,
    text: String,
}

/// A call edge `caller -> callee` recorded at a call site inside `caller`.
struct Edge {
    caller: String,
    callee: String,
    line: usize,
    text: String,
}

/// Everything `call_graph` needs from one file, independent of the query symbol.
#[derive(Default)]
struct Distilled {
    defs: Vec<DefSite>,
    edges: Vec<Edge>,
}

/// Distilled graphs persisted across spawns: defs + edges are small (unlike
/// `symbol_refs`' full occurrence lists), so they are cheap to keep on disk,
/// keyed path+mtime.
static CG_CACHE: OnceLock<Mutex<DiskMap>> = OnceLock::new();

pub fn build(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let direction = args
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("both")
        .to_lowercase();
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(60).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let want_callers = direction == "both" || direction == "callers";
    let want_callees = direction == "both" || direction == "callees";

    let Hop {
        defs,
        callees,
        callers,
        scanned_bytes,
    } = gather(root, symbol, &paths, want_callers, want_callees);

    let out = render(
        symbol,
        &paths,
        &direction,
        want_callers,
        want_callees,
        &defs,
        &callees,
        &callers,
        max,
        budget,
    );
    stats::record("call_graph", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// The query-filtered hop in `call_graph`'s native (private) shapes, ready for
/// `render`. Factored out of `build` so other tools can reuse the same walk.
struct Hop {
    defs: Vec<Site>,
    callees: BTreeSet<String>,
    callers: BTreeMap<String, Site>,
    scanned_bytes: u64,
}

/// Walk `paths`, distill every file (warm via the disk cache, cold in parallel),
/// and filter the graphs down to `symbol`'s defs, callees, and callers. This is
/// steps 1–5 of the original `build`, lifted verbatim so behaviour is identical.
fn gather(
    root: &Path,
    symbol: &str,
    paths: &[String],
    want_callers: bool,
    want_callees: bool,
) -> Hop {
    // Steps 1–4: enumerate, warm/cold split, parse, gather distilled graphs.
    let (per_file, scanned_bytes) = distilled_files(root, paths);

    // 5. Filter the distilled graphs by the query symbol.
    let mut defs: Vec<Site> = Vec::new();
    let mut callees: BTreeSet<String> = BTreeSet::new();
    let mut callers: BTreeMap<String, Site> = BTreeMap::new();
    for (rel, d) in &per_file {
        for ds in &d.defs {
            if name_matches(&ds.name, symbol) {
                defs.push(Site {
                    rel: rel.clone(),
                    line: ds.line,
                    text: ds.text.clone(),
                });
            }
        }
        for e in &d.edges {
            if want_callers && name_matches(&e.callee, symbol) && !name_matches(&e.caller, symbol) {
                callers.entry(e.caller.clone()).or_insert_with(|| Site {
                    rel: rel.clone(),
                    line: e.line,
                    text: e.text.clone(),
                });
            }
            if want_callees && name_matches(&e.caller, symbol) && !name_matches(&e.callee, symbol) {
                callees.insert(e.callee.clone());
            }
        }
    }

    Hop {
        defs,
        callees,
        callers,
        scanned_bytes,
    }
}

/// Steps 1–4 of the walk shared by [`gather`] and [`adjacency`]: enumerate
/// candidate files, warm/cold split against the disk cache, parse cold files in
/// parallel (publishing + persisting), and return every file's distilled graph
/// (sorted by rel for deterministic tie-breaks) plus the bytes scanned.
fn distilled_files(root: &Path, paths: &[String]) -> (Vec<(String, Distilled)>, u64) {
    // 1. Candidate files (path, language, mtime). Daslang is out of scope here;
    //    `repo_map` maps its declarations instead.
    let prune = walk::prune_set(root);
    let mut candidates: Vec<(PathBuf, Lang, u64)> = Vec::new();
    let mut scanned_bytes = 0u64;
    for p in paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            if !cg_supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            candidates.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }

    // 2. Warm/cold split against the on-disk cache.
    let cell = CG_CACHE.get_or_init(|| Mutex::new(DiskMap::load("call_graph.json")));
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, lang, mt) in &candidates {
            if store.get(path.to_string_lossy().as_ref(), *mt).is_none() {
                cold.push((path.clone(), *lang, *mt));
            }
        }
    }

    // 3. Parse cold files in parallel, then publish + persist.
    let fresh = parallel_extract(&cold);
    {
        let mut store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, mt, d) in &fresh {
            store.put(path.to_string_lossy().as_ref(), *mt, encode(d));
        }
        store.save();
    }

    // 4. Gather every candidate's distilled graph, sorted by rel so the "first
    //    caller site" tie-break is deterministic regardless of thread timing.
    let mut per_file: Vec<(String, Distilled)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, _lang, mt) in &candidates {
            if let Some(v) = store.get(path.to_string_lossy().as_ref(), *mt) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                per_file.push((rel, decode(v)));
            }
        }
    }
    per_file.sort_by(|a, b| a.0.cmp(&b.0));
    (per_file, scanned_bytes)
}

/// One hop of the call graph in plain owned shapes, for other tools
/// (`symbol_context`, `diff_map`) to compose without touching the private
/// `Site`/`Distilled` types. Always gathers both directions.
pub struct OneHop {
    /// Names of functions the symbol calls.
    pub callees: Vec<String>,
    /// Functions that call the symbol: `(caller_name, rel, line)`.
    pub callers: Vec<(String, String, usize)>,
    /// Bytes scanned, for the savings counter.
    pub scanned_bytes: u64,
}

/// Public entry for `symbol_context`/`diff_map`: callers + callees of `symbol`.
pub fn one_hop(root: &Path, symbol: &str, paths: &[String]) -> OneHop {
    let h = gather(root, symbol, paths, true, true);
    OneHop {
        callees: h.callees.into_iter().collect(),
        callers: h
            .callers
            .into_iter()
            .map(|(name, s)| (name, s.rel, s.line))
            .collect(),
        scanned_bytes: h.scanned_bytes,
    }
}

/// Whole-tree call adjacency, built once from the distilled per-file graphs
/// (warm via the disk cache). Names are normalized to their trailing `::`
/// segment so `Foo::Bar` and `Bar` unify, matching the rest of `call_graph`.
/// Powers `call_path`'s BFS and `undocumented`'s fan-in ranking.
pub struct Adjacency {
    /// caller segment → the callee segments it invokes.
    pub callees: BTreeMap<String, BTreeSet<String>>,
    /// callee segment → the caller segments that invoke it.
    pub callers: BTreeMap<String, BTreeSet<String>>,
    /// One representative definition site per segment: name → (rel, line).
    pub sites: BTreeMap<String, (String, usize)>,
    /// Bytes scanned, for the savings counter.
    pub scanned_bytes: u64,
}

/// Build the whole-tree call [`Adjacency`] over `paths`.
pub fn adjacency(root: &Path, paths: &[String]) -> Adjacency {
    let (per_file, scanned_bytes) = distilled_files(root, paths);
    let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut callers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut sites: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for (rel, d) in &per_file {
        for ds in &d.defs {
            sites
                .entry(seg(&ds.name).to_string())
                .or_insert_with(|| (rel.clone(), ds.line));
        }
        for e in &d.edges {
            let cr = seg(&e.caller).to_string();
            let ce = seg(&e.callee).to_string();
            if cr == ce {
                continue; // skip self-recursion, matching gather's caller≠callee filter
            }
            callees.entry(cr.clone()).or_default().insert(ce.clone());
            callers.entry(ce).or_default().insert(cr);
        }
    }
    Adjacency {
        callees,
        callers,
        sites,
        scanned_bytes,
    }
}

impl Adjacency {
    /// Callers of `symbol` as `(caller_name, rel, line)`, matching [`one_hop`].
    pub fn callers_of(&self, symbol: &str) -> Vec<(String, String, usize)> {
        let key = seg(symbol);
        let Some(cs) = self.callers.get(key) else {
            return Vec::new();
        };
        let mut out: Vec<(String, String, usize)> = cs
            .iter()
            .map(|name| {
                let (rel, line) = self
                    .sites
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| ("?".into(), 0));
                (name.clone(), rel, line)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        out
    }
}

/// Languages `call_graph` walks. Daslang is excluded (brace-only grammar; its
/// scripts are mapped by `repo_map`).
fn cg_supported(lang: Lang) -> bool {
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

/// Distill the cold files into call graphs, in parallel. Each thread owns a
/// `Parser` (tree-sitter parsers are not `Sync`).
fn parallel_extract(cold: &[(PathBuf, Lang, u64)]) -> Vec<(PathBuf, u64, Distilled)> {
    if cold.is_empty() {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(cold.len())
        .min(8);
    if threads <= 1 {
        let mut parser = Parser::new();
        return cold
            .iter()
            .map(|(p, l, mt)| (p.clone(), *mt, extract_one(&mut parser, *l, p)))
            .collect();
    }
    let chunk = cold.len().div_ceil(threads);
    let mut out = Vec::with_capacity(cold.len());
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for ck in cold.chunks(chunk) {
            handles.push(s.spawn(move || {
                let mut parser = Parser::new();
                ck.iter()
                    .map(|(p, l, mt)| (p.clone(), *mt, extract_one(&mut parser, *l, p)))
                    .collect::<Vec<_>>()
            }));
        }
        for h in handles {
            if let Ok(part) = h.join() {
                out.extend(part);
            }
        }
    });
    out
}

fn extract_one(parser: &mut Parser, lang: Lang, path: &Path) -> Distilled {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Distilled::default();
    };
    extract_graph(parser, lang, &src)
}

/// Distill a file into all function defs and call edges, symbol-agnostic.
fn extract_graph(parser: &mut Parser, lang: Lang, src: &str) -> Distilled {
    let Some(ts) = lang.ts_language() else {
        return Distilled::default();
    };
    if parser.set_language(&ts).is_err() {
        return Distilled::default();
    }
    let prepared = lang.preprocess(src);
    let Some(tree) = parser.parse(prepared.as_ref(), None) else {
        return Distilled::default();
    };
    let bytes = prepared.as_bytes();
    let lines: Vec<&str> = prepared.lines().collect();
    let mut d = Distilled::default();
    match lang {
        Lang::Rust => visit_rust(tree.root_node(), bytes, &lines, None, &mut d),
        Lang::Python => visit_python(tree.root_node(), bytes, &lines, None, &mut d),
        Lang::Ts | Lang::Tsx | Lang::Svelte => {
            visit_ts(tree.root_node(), bytes, &lines, None, &mut d)
        }
        Lang::CSharp => visit_csharp(tree.root_node(), bytes, &lines, None, &mut d),
        // C/C++ and GLSL (a tree-sitter-c fork) share the C node kinds.
        _ => visit_cpp(tree.root_node(), bytes, &lines, None, &mut d),
    }
    d
}

fn encode(d: &Distilled) -> Value {
    let defs: Vec<Value> = d
        .defs
        .iter()
        .map(|x| json!([x.name, x.line, x.text]))
        .collect();
    let edges: Vec<Value> = d
        .edges
        .iter()
        .map(|x| json!([x.caller, x.callee, x.line, x.text]))
        .collect();
    json!({ "defs": defs, "edges": edges })
}

fn decode(v: &Value) -> Distilled {
    let defs = v
        .get("defs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let t = e.as_array()?;
                    Some(DefSite {
                        name: t.first()?.as_str()?.to_string(),
                        line: t.get(1)?.as_u64()? as usize,
                        text: t.get(2)?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let edges = v
        .get("edges")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let t = e.as_array()?;
                    Some(Edge {
                        caller: t.first()?.as_str()?.to_string(),
                        callee: t.get(1)?.as_str()?.to_string(),
                        line: t.get(2)?.as_u64()? as usize,
                        text: t.get(3)?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Distilled { defs, edges }
}

/// Walk a C/C++/GLSL tree tracking the nearest enclosing function (`current`),
/// recording every function definition and every `caller -> callee` call edge.
fn visit_cpp(node: Node, bytes: &[u8], lines: &[&str], current: Option<&str>, d: &mut Distilled) {
    let mut cur = current;
    let mut owned: Option<String> = None;

    if node.kind() == "function_definition" {
        if let Some(name) = function_name(node, bytes) {
            let row = node.start_position().row;
            d.defs.push(DefSite {
                name: name.clone(),
                line: row + 1,
                text: line_text(lines, row),
            });
            owned = Some(name);
        }
    }
    if let Some(ref n) = owned {
        cur = Some(n.as_str());
    }

    if node.kind() == "call_expression" {
        if let (Some(callee), Some(c)) = (callee_name(node, bytes), cur) {
            let row = node.start_position().row;
            d.edges.push(Edge {
                caller: c.to_string(),
                callee,
                line: row + 1,
                text: line_text(lines, row),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_cpp(child, bytes, lines, cur, d);
    }
}

/// Rust analogue of `visit_cpp`: `function_item` defs, plus `call_expression`
/// and `macro_invocation` edges.
fn visit_rust(node: Node, bytes: &[u8], lines: &[&str], current: Option<&str>, d: &mut Distilled) {
    let mut cur = current;
    let mut owned: Option<String> = None;

    if node.kind() == "function_item" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = text(name_node, bytes).to_string();
            let row = node.start_position().row;
            d.defs.push(DefSite {
                name: name.clone(),
                line: row + 1,
                text: line_text(lines, row),
            });
            owned = Some(name);
        }
    }
    if let Some(ref n) = owned {
        cur = Some(n.as_str());
    }

    let callee = match node.kind() {
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(|f| rust_callee(f, bytes)),
        "macro_invocation" => node
            .child_by_field_name("macro")
            .map(|m| text(m, bytes).to_string()),
        _ => None,
    };
    if let (Some(callee), Some(c)) = (callee, cur) {
        let row = node.start_position().row;
        d.edges.push(Edge {
            caller: c.to_string(),
            callee,
            line: row + 1,
            text: line_text(lines, row),
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_rust(child, bytes, lines, cur, d);
    }
}

/// The callee name of a Rust call function expression.
fn rust_callee(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "scoped_identifier" | "field_identifier" => {
            Some(text(node, bytes).to_string())
        }
        "field_expression" => node
            .child_by_field_name("field")
            .map(|n| text(n, bytes).to_string()),
        "generic_function" => node
            .child_by_field_name("function")
            .and_then(|n| rust_callee(n, bytes)),
        _ => None,
    }
}

/// Python analogue of `visit_cpp`: `function_definition` defs, plus `call`
/// edges (free `f()` and attribute `o.f()`).
fn visit_python(
    node: Node,
    bytes: &[u8],
    lines: &[&str],
    current: Option<&str>,
    d: &mut Distilled,
) {
    let mut cur = current;
    let mut owned: Option<String> = None;

    if node.kind() == "function_definition" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = text(name_node, bytes).to_string();
            let row = node.start_position().row;
            d.defs.push(DefSite {
                name: name.clone(),
                line: row + 1,
                text: line_text(lines, row),
            });
            owned = Some(name);
        }
    }
    if let Some(ref n) = owned {
        cur = Some(n.as_str());
    }

    if node.kind() == "call" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|f| match f.kind() {
                "identifier" => Some(text(f, bytes).to_string()),
                "attribute" => f
                    .child_by_field_name("attribute")
                    .map(|n| text(n, bytes).to_string()),
                _ => None,
            });
        if let (Some(callee), Some(c)) = (callee, cur) {
            let row = node.start_position().row;
            d.edges.push(Edge {
                caller: c.to_string(),
                callee,
                line: row + 1,
                text: line_text(lines, row),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_python(child, bytes, lines, cur, d);
    }
}

/// TypeScript/TSX/Svelte analogue of `visit_cpp`: function declarations, class
/// methods, and `const f = () => {}` bindings as defs, plus `call_expression`
/// edges (free `f()` and member `o.f()`). Svelte is pre-reduced to its
/// `<script>` by `extract_graph` before this runs.
fn visit_ts(node: Node, bytes: &[u8], lines: &[&str], current: Option<&str>, d: &mut Distilled) {
    let mut cur = current;
    let mut owned: Option<String> = None;

    let def_name: Option<String> = match node.kind() {
        "function_declaration" | "generator_function_declaration" | "method_definition" => node
            .child_by_field_name("name")
            .map(|n| text(n, bytes).to_string()),
        "variable_declarator" | "public_field_definition" => {
            let is_fn = node
                .child_by_field_name("value")
                .map(|v| {
                    matches!(
                        v.kind(),
                        "arrow_function" | "function" | "function_expression"
                    )
                })
                .unwrap_or(false);
            if is_fn {
                node.child_by_field_name("name")
                    .map(|n| text(n, bytes).to_string())
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(name) = def_name {
        let row = node.start_position().row;
        d.defs.push(DefSite {
            name: name.clone(),
            line: row + 1,
            text: line_text(lines, row),
        });
        owned = Some(name);
    }
    if let Some(ref n) = owned {
        cur = Some(n.as_str());
    }

    if node.kind() == "call_expression" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|f| ts_callee(f, bytes));
        if let (Some(callee), Some(c)) = (callee, cur) {
            let row = node.start_position().row;
            d.edges.push(Edge {
                caller: c.to_string(),
                callee,
                line: row + 1,
                text: line_text(lines, row),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_ts(child, bytes, lines, cur, d);
    }
}

/// The callee name of a TypeScript call function expression: free `f()` or
/// member `obj.f()` (the trailing property).
fn ts_callee(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, bytes).to_string()),
        "member_expression" => node
            .child_by_field_name("property")
            .map(|n| text(n, bytes).to_string()),
        _ => None,
    }
}

/// C# analogue of `visit_ts`: method/constructor defs plus `invocation_expression`
/// edges (free `f()` and member `obj.f()`).
fn visit_csharp(
    node: Node,
    bytes: &[u8],
    lines: &[&str],
    current: Option<&str>,
    d: &mut Distilled,
) {
    let mut cur = current;
    let mut owned: Option<String> = None;

    let def_name: Option<String> = match node.kind() {
        "method_declaration" | "constructor_declaration" | "destructor_declaration" => node
            .child_by_field_name("name")
            .map(|n| text(n, bytes).to_string()),
        _ => None,
    };
    if let Some(name) = def_name {
        let row = node.start_position().row;
        d.defs.push(DefSite {
            name: name.clone(),
            line: row + 1,
            text: line_text(lines, row),
        });
        owned = Some(name);
    }
    if let Some(ref n) = owned {
        cur = Some(n.as_str());
    }

    if node.kind() == "invocation_expression" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|f| csharp_callee(f, bytes));
        if let (Some(callee), Some(c)) = (callee, cur) {
            let row = node.start_position().row;
            d.edges.push(Edge {
                caller: c.to_string(),
                callee,
                line: row + 1,
                text: line_text(lines, row),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_csharp(child, bytes, lines, cur, d);
    }
}

/// The callee name of a C# invocation: free `f()` or member `obj.f()`.
fn csharp_callee(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, bytes).to_string()),
        "member_access_expression" => node
            .child_by_field_name("name")
            .map(|n| text(n, bytes).to_string()),
        "generic_name" => node
            .child_by_field_name("name")
            .map(|n| text(n, bytes).to_string()),
        _ => None,
    }
}

/// The declared name of a `function_definition`, e.g. `foo` or `Foo::Bar`.
fn function_name(def: Node, bytes: &[u8]) -> Option<String> {
    let decl = def.child_by_field_name("declarator")?;
    let fdecl = find_function_declarator(decl)?;
    let name = fdecl.child_by_field_name("declarator")?;
    Some(text(name, bytes).to_string())
}

/// Descend pointer/reference/parenthesized declarators to the function_declarator.
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

/// The callee name of a `call_expression`: free `foo()`, member `obj.foo()`,
/// static `Foo::foo()`, or templated `foo<T>()`.
fn callee_name(call: Node, bytes: &[u8]) -> Option<String> {
    let f = call.child_by_field_name("function")?;
    callee_from(f, bytes)
}

fn callee_from(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "qualified_identifier" | "field_identifier" => {
            Some(text(node, bytes).to_string())
        }
        "field_expression" => node
            .child_by_field_name("field")
            .map(|n| text(n, bytes).to_string()),
        "template_function" => node
            .child_by_field_name("name")
            .and_then(|n| callee_from(n, bytes)),
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find_map(|c| callee_from(c, bytes));
            found
        }
        _ => None,
    }
}

/// Last `::`-separated segment of a (possibly qualified) name.
pub(crate) fn seg(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Match on the full name or its trailing segment, so a plain query lines up
/// with qualified call sites and out-of-line member definitions and vice versa.
fn name_matches(a: &str, b: &str) -> bool {
    a == b || seg(a) == seg(b)
}

#[allow(clippy::too_many_arguments)]
fn render(
    symbol: &str,
    paths: &[String],
    direction: &str,
    want_callers: bool,
    want_callees: bool,
    defs: &[Site],
    callees: &BTreeSet<String>,
    callers: &BTreeMap<String, Site>,
    max: usize,
    budget: usize,
) -> String {
    let mut out = format!("call_graph - \"{symbol}\"  (roots {paths:?}, direction {direction})\n");
    let mut used = out.len() / 4;

    out.push_str(&format!("\ndefinitions ({}):\n", defs.len()));
    if defs.is_empty() {
        out.push_str("  (no definition found in scope)\n");
    } else {
        for d in defs.iter().take(max) {
            out.push_str(&format!("  {}:{}  {}\n", d.rel, d.line, d.text));
        }
    }

    if want_callees {
        out.push_str(&format!(
            "\ncallees - functions {symbol} calls ({}):\n",
            callees.len()
        ));
        if callees.is_empty() {
            out.push_str(if defs.is_empty() {
                "  (no definition in scope to read)\n"
            } else {
                "  (none)\n"
            });
        } else {
            for (i, c) in callees.iter().enumerate() {
                if i >= max {
                    out.push_str(&format!("  … (+{} more)\n", callees.len() - max));
                    break;
                }
                out.push_str(&format!("  {c}\n"));
            }
        }
    }

    if want_callers {
        out.push_str(&format!(
            "\ncallers - functions that call {symbol} ({}):\n",
            callers.len()
        ));
        if callers.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for (i, (name, site)) in callers.iter().enumerate() {
                if i >= max {
                    out.push_str(&format!("  … (+{} more)\n", callers.len() - max));
                    break;
                }
                let line = format!("  {name}   {}:{}\n", site.rel, site.line);
                used += line.len() / 4;
                if used > budget && i > 0 {
                    out.push_str("  … (truncated by token_budget)\n");
                    break;
                }
                out.push_str(&line);
            }
        }
    }
    out
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// The source line at 0-based `row`, whitespace-collapsed and length-capped.
fn line_text(lines: &[&str], row: usize) -> String {
    let raw = lines.get(row).copied().unwrap_or("").trim();
    let one = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 100 {
        one
    } else {
        let t: String = one.chars().take(99).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_in(
        src: &str,
        symbol: &str,
        lang: Lang,
    ) -> (Vec<String>, BTreeSet<String>, Vec<String>) {
        let mut p = Parser::new();
        let d = extract_graph(&mut p, lang, src);
        let def_names: Vec<String> = d
            .defs
            .iter()
            .filter(|ds| name_matches(&ds.name, symbol))
            .map(|ds| format!("t:{}", ds.line))
            .collect();
        let mut callees = BTreeSet::new();
        let mut callers = BTreeSet::new();
        for e in &d.edges {
            if name_matches(&e.callee, symbol) && !name_matches(&e.caller, symbol) {
                callers.insert(e.caller.clone());
            }
            if name_matches(&e.caller, symbol) && !name_matches(&e.callee, symbol) {
                callees.insert(e.callee.clone());
            }
        }
        (def_names, callees, callers.into_iter().collect())
    }

    fn graph(src: &str, symbol: &str) -> (Vec<String>, BTreeSet<String>, Vec<String>) {
        graph_in(src, symbol, Lang::Cpp)
    }

    #[test]
    fn finds_callees_within_target_body() {
        let src = "int helper_a();\nint helper_b();\nint target() {\n  int x = helper_a();\n  return helper_b() + x;\n}\n";
        let (_defs, callees, _callers) = graph(src, "target");
        assert!(callees.contains("helper_a"));
        assert!(callees.contains("helper_b"));
    }

    #[test]
    fn finds_callers_of_target() {
        let src =
            "int target();\nint run() {\n  return target();\n}\nint other() { return run(); }\n";
        let (_defs, _callees, callers) = graph(src, "target");
        assert_eq!(callers, vec!["run".to_string()]);
    }

    #[test]
    fn matches_qualified_and_member_calls() {
        let src = "struct S { void poke(); };\nvoid driver(S& s) {\n  s.poke();\n}\nvoid Foo::bar() { driver_unused(); }\n";
        // Member call `s.poke()` should attribute `driver` as a caller of `poke`.
        let (_defs, _callees, callers) = graph(src, "poke");
        assert!(callers.contains(&"driver".to_string()));
    }

    #[test]
    fn out_of_line_member_def_is_found_and_self_calls_excluded() {
        let src = "void Foo::Bar() {\n  Bar();\n  helper();\n}\n";
        let (defs, callees, callers) = graph(src, "Bar");
        assert_eq!(defs.len(), 1); // the out-of-line definition
        assert!(callees.contains("helper"));
        // self-recursive call is not listed as a callee or an external caller
        assert!(!callees.contains("Bar"));
        assert!(callers.is_empty());
    }

    #[test]
    fn seg_and_name_matches() {
        assert_eq!(seg("Foo::Bar"), "Bar");
        assert_eq!(seg("plain"), "plain");
        assert!(name_matches("Foo::Bar", "Bar"));
        assert!(name_matches("Bar", "Foo::Bar"));
        assert!(!name_matches("Foo::Baz", "Bar"));
    }

    #[test]
    fn callers_of_uses_sites_from_adjacency() {
        let mut callers = BTreeMap::new();
        callers.insert(
            "target".into(),
            ["run".into(), "tick".into()].into_iter().collect(),
        );
        let mut sites = BTreeMap::new();
        sites.insert("run".into(), ("src/a.cpp".into(), 10));
        sites.insert("tick".into(), ("src/b.cpp".into(), 20));
        let adj = Adjacency {
            callees: BTreeMap::new(),
            callers,
            sites,
            scanned_bytes: 42,
        };
        let list = adj.callers_of("ns::target");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, "run");
        assert_eq!(list[0].1, "src/a.cpp");
        assert_eq!(list[1].0, "tick");
    }

    #[test]
    fn rust_callees_and_callers() {
        let src = "fn helper_a() {}\nfn helper_b() {}\nfn target() {\n  helper_a();\n  helper_b();\n}\nfn run() { target(); }\n";
        let (_defs, callees, callers) = graph_in(src, "target", Lang::Rust);
        assert!(callees.contains("helper_a"), "{callees:?}");
        assert!(callees.contains("helper_b"), "{callees:?}");
        assert_eq!(callers, vec!["run".to_string()]);
    }

    #[test]
    fn python_callees_and_callers() {
        let src =
            "def helper():\n    pass\n\ndef target():\n    helper()\n\ndef run():\n    target()\n";
        let (_defs, callees, callers) = graph_in(src, "target", Lang::Python);
        assert!(callees.contains("helper"), "{callees:?}");
        assert_eq!(callers, vec!["run".to_string()]);
    }

    #[test]
    fn ts_callees_and_callers() {
        let src = "function helper() {}\nfunction target() {\n  helper();\n}\nfunction run() { target(); }\n";
        let (_defs, callees, callers) = graph_in(src, "target", Lang::Ts);
        assert!(callees.contains("helper"), "{callees:?}");
        assert_eq!(callers, vec!["run".to_string()]);
    }

    #[test]
    fn csharp_callees_and_callers() {
        let src = "class Plugin {\n  void Awake() { Logger.LogInfo(\"hi\"); }\n}\nclass Runner {\n  void Run() { var p = new Plugin(); p.Awake(); }\n}\n";
        let (_defs, callees, callers) = graph_in(src, "Awake", Lang::CSharp);
        assert!(callees.contains("LogInfo"), "{callees:?}");
        assert!(callers.contains(&"Run".to_string()), "{callers:?}");
    }
}
