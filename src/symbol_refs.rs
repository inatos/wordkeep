//! symbol_refs - locate where a symbol is defined, called, and referenced.
//!
//! tree-sitter classifies each exact-name occurrence as a definition, a call
//! site, or some other reference - far less noisy than grep (it ignores
//! comments, ordinary strings, and substring collisions). Coverage spans C/C++,
//! GLSL (a tree-sitter-c fork sharing the C node kinds), Rust, Python, and
//! TypeScript/TSX/Svelte/JS (Svelte parsed through its `<script>` blocks);
//! Daslang is mapped by `repo_map` instead (its grammar is brace-only and its
//! scripts are small). Flecs `ecs.system("…")` and Tracy `ZoneScopedN("…")`
//! string names are the deliberate exception: they count as definitions.
//!
//! Each file is distilled once into a symbol-agnostic occurrence list
//! (name, role, line) memoized per process keyed by path+mtime, so the parse is
//! paid only on the first query that touches a file; cold files are parsed in
//! parallel. The first query over a tree warms the memo, and later queries for
//! any symbol just filter the cached lists.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Node, Parser};

use crate::continuation::{self, KIND_HIT_SKIP};
use crate::lang::Lang;
use crate::{cache, progress, stats, walk};

// Leaf name nodes plus `A::b` (qualified) so scope-qualified queries resolve too.
const NAME_KINDS_CPP: [&str; 5] = [
    "identifier",
    "field_identifier",
    "type_identifier",
    "namespace_identifier",
    "qualified_identifier",
];

type OccCacheMap = HashMap<String, (u64, Vec<Occ>)>;

/// Process-lifetime memo: absolute path → (mtime_ns, all occurrences). The
/// occurrence list is large but ephemeral, so it lives in memory only (unlike
/// `call_graph`, whose distilled edges are small enough to persist on disk).
static OCC_CACHE: OnceLock<Mutex<OccCacheMap>> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    Def,
    Call,
    Ref,
}

/// One classified name occurrence, independent of any query symbol. Files are
/// distilled into these once and memoized; a query just filters by `name`.
#[derive(Clone)]
struct Occ {
    name: String,
    role: Role,
    line: usize,
}

struct Hit {
    role: Role,
    rel: String,
    line: usize,
    text: String,
}

pub fn find(root: &Path, args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if symbol.is_empty() {
        return Err(crate::config::symbol_required_err());
    }
    let paths = crate::config::paths_from_args(root, args)?;
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .to_lowercase();
    let max = args.get("max").and_then(Value::as_u64).unwrap_or(60).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    const FP_KEYS: &[&str] = &["symbol", "paths", "profile", "kind", "max"];
    let args_fp = continuation::args_fingerprint(args, FP_KEYS);
    let resume_at =
        continuation::resume_offset(args, "symbol_refs", FP_KEYS, KIND_HIT_SKIP)? as usize;
    // Fast path: when only definitions are wanted, drop calls/refs while filtering.
    let defs_only = kind == "def";

    progress::tick(0, None, "symbol_refs: scanning");

    let want = |r: Role| match kind.as_str() {
        "def" => r == Role::Def,
        "call" => r == Role::Call,
        "ref" => r == Role::Ref,
        _ => true,
    };

    // 1. Enumerate candidate files (path, language, mtime). Languages without a
    //    classifier here (Daslang) are skipped; `repo_map` covers those.
    let prune = walk::prune_set(root);
    let mut candidates: Vec<(PathBuf, Lang, u64)> = Vec::new();
    let mut scanned_bytes = 0u64;
    for p in &paths {
        let base = root.join(p);
        for entry in walk::files(&base, &prune) {
            let path = entry.path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            if !refs_supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            // File sizes are the raw material an agent would otherwise grep, so
            // they form the stats baseline whether or not the parse is cached.
            scanned_bytes += meta.len();
            candidates.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }

    // 2. Split warm (memoized, mtime match) from cold (needs a parse).
    let cell = OCC_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, lang, mt) in &candidates {
            let key = path.to_string_lossy();
            match store.get(key.as_ref()) {
                Some((cached_mt, _)) if cached_mt == mt => {}
                _ => cold.push((path.clone(), *lang, *mt)),
            }
        }
    }

    // 3. Parse cold files in parallel, then publish into the memo.
    let fresh = parallel_extract(&cold);
    {
        let mut store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, mt, occ) in fresh {
            store.insert(path.to_string_lossy().into_owned(), (mt, occ));
        }
    }

    // 4. Filter every candidate's occurrences by the query symbol (CPU only,
    //    under the lock); clone out matches so file re-reads happen lock-free.
    let mut matched: Vec<(PathBuf, Vec<Occ>)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, _lang, mt) in &candidates {
            let key = path.to_string_lossy();
            if let Some((cached_mt, occ)) = store.get(key.as_ref()) {
                if cached_mt != mt {
                    continue;
                }
                let owned: Vec<Occ> = occ
                    .iter()
                    .filter(|o| o.name == symbol && (!defs_only || o.role == Role::Def))
                    .cloned()
                    .collect();
                if !owned.is_empty() {
                    matched.push((path.clone(), owned));
                }
            }
        }
    }

    // 5. Build display hits, reading each matched file once for line text.
    let mut hits: Vec<Hit> = Vec::new();
    let files_scanned = matched.len();
    for (path, occ) in &matched {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<&str> = src.lines().collect();
        for o in occ {
            hits.push(Hit {
                role: o.role,
                rel: rel.clone(),
                line: o.line,
                text: line_text(&lines, o.line.saturating_sub(1)),
            });
        }
    }

    // Stable, useful order: defs, then calls, then refs; each by path/line.
    let rank = |r: Role| match r {
        Role::Def => 0,
        Role::Call => 1,
        Role::Ref => 2,
    };
    hits.sort_by(|a, b| {
        rank(a.role)
            .cmp(&rank(b.role))
            .then_with(|| a.rel.cmp(&b.rel))
            .then_with(|| a.line.cmp(&b.line))
    });

    let (mut nd, mut nc, mut nr) = (0usize, 0usize, 0usize);
    for h in &hits {
        match h.role {
            Role::Def => nd += 1,
            Role::Call => nc += 1,
            Role::Ref => nr += 1,
        }
    }

    let mut out = if defs_only {
        format!("symbol_refs - \"{symbol}\": {nd} def across {files_scanned} file(s) (definition-only)")
    } else {
        format!(
            "symbol_refs - \"{symbol}\": {nd} def, {nc} call, {nr} ref across {files_scanned} file(s)"
        )
    };
    if resume_at > 0 {
        out.push_str(&format!(" [continuation from hit #{resume_at}]"));
    }
    out.push('\n');
    if kind != "all" && !defs_only {
        out.push_str(&format!("filter: {kind}\n"));
    }
    out.push('\n');

    let mut used = out.len() / 4;
    let mut shown = 0usize;
    let mut hit_index = 0usize;
    let mut truncated_at: Option<usize> = None;
    let mut last_role: Option<Role> = None;
    for h in hits.iter().filter(|h| want(h.role)) {
        if hit_index < resume_at {
            hit_index += 1;
            continue;
        }
        if shown >= max {
            out.push_str("… (more hits; raise \"max\")\n");
            break;
        }
        if last_role != Some(h.role) {
            out.push_str(&format!("{}:\n", label(h.role)));
            last_role = Some(h.role);
        }
        let line = format!("  {}:{}  {}\n", h.rel, h.line, h.text);
        let lt = line.len() / 4;
        if used + lt > budget && shown > 0 {
            truncated_at = Some(hit_index);
            break;
        }
        used += lt;
        shown += 1;
        hit_index += 1;
        out.push_str(&line);
    }

    if shown == 0 && resume_at == 0 {
        out.push_str("(no matching occurrences)\n");
        out.push_str(&crate::symbol_resolve::did_you_mean_hint(root, symbol, &paths));
    }
    if let Some(idx) = truncated_at {
        continuation::append_footer(
            &mut out,
            "symbol_refs",
            args_fp,
            idx as u64,
            KIND_HIT_SKIP,
            "more hits",
        );
    }
    progress::tick(shown as u64, None, "symbol_refs: done");
    stats::record("symbol_refs", scanned_bytes / 4, (out.len() / 4) as u64);
    Ok(out)
}

/// One file's occurrence counts for a symbol, used by `test_map`.
pub struct FileRefs {
    pub rel: String,
    pub defs: usize,
    pub calls: usize,
    pub refs: usize,
}

/// Per-file occurrence counts for `symbol` under `paths`, reusing the same memo
/// and parallel parse as [`find`]. Files with no match are omitted; results are
/// sorted by total hits (desc) then path. The second tuple element is the total
/// bytes scanned (the raw material an agent would otherwise open) so `test_map`
/// can report honest savings. Powers `test_map`.
pub fn refs_by_file(root: &Path, symbol: &str, paths: &[String]) -> (Vec<FileRefs>, u64) {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return (Vec::new(), 0);
    }

    // 1. Enumerate candidate files (same gate as `find`).
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
            if !refs_supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            candidates.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }

    // 2. Warm/cold split against the process memo.
    let cell = OCC_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, lang, mt) in &candidates {
            let key = path.to_string_lossy();
            match store.get(key.as_ref()) {
                Some((cached_mt, _)) if cached_mt == mt => {}
                _ => cold.push((path.clone(), *lang, *mt)),
            }
        }
    }

    // 3. Parse cold files in parallel, then publish.
    let fresh = parallel_extract(&cold);
    {
        let mut store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, mt, occ) in fresh {
            store.insert(path.to_string_lossy().into_owned(), (mt, occ));
        }
    }

    // 4. Tally per file.
    let mut out: Vec<FileRefs> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, _lang, mt) in &candidates {
            let key = path.to_string_lossy();
            if let Some((cached_mt, occ)) = store.get(key.as_ref()) {
                if cached_mt != mt {
                    continue;
                }
                let (mut d, mut c, mut r) = (0usize, 0usize, 0usize);
                for o in occ.iter().filter(|o| o.name == symbol) {
                    match o.role {
                        Role::Def => d += 1,
                        Role::Call => c += 1,
                        Role::Ref => r += 1,
                    }
                }
                if d + c + r > 0 {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push(FileRefs {
                        rel,
                        defs: d,
                        calls: c,
                        refs: r,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        (b.defs + b.calls + b.refs)
            .cmp(&(a.defs + a.calls + a.refs))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    (out, scanned_bytes)
}

/// One call site of a symbol: file + 1-based line. Powers `usage_examples`.
pub struct CallSite {
    pub rel: String,
    pub line: usize,
}

/// Every call site of `symbol` under `paths`, reusing the same memo + parallel
/// parse as [`find`] (so it shares warm occurrence lists). Sorted by path then
/// line; the second tuple element is the bytes scanned (the raw material an
/// agent would otherwise grep) so `usage_examples` can report honest savings.
pub fn call_sites(root: &Path, symbol: &str, paths: &[String]) -> (Vec<CallSite>, u64) {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return (Vec::new(), 0);
    }

    // 1. Candidate files (same gate as `find`).
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
            if !refs_supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            candidates.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }

    // 2. Warm/cold split against the process memo.
    let cell = OCC_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, lang, mt) in &candidates {
            let key = path.to_string_lossy();
            match store.get(key.as_ref()) {
                Some((cached_mt, _)) if cached_mt == mt => {}
                _ => cold.push((path.clone(), *lang, *mt)),
            }
        }
    }

    // 3. Parse cold files in parallel, then publish.
    let fresh = parallel_extract(&cold);
    {
        let mut store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, mt, occ) in fresh {
            store.insert(path.to_string_lossy().into_owned(), (mt, occ));
        }
    }

    // 4. Collect every call occurrence of the symbol.
    let mut out: Vec<CallSite> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, _lang, mt) in &candidates {
            let key = path.to_string_lossy();
            if let Some((cached_mt, occ)) = store.get(key.as_ref()) {
                if cached_mt != mt {
                    continue;
                }
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                for o in occ
                    .iter()
                    .filter(|o| o.name == symbol && o.role == Role::Call)
                {
                    out.push(CallSite {
                        rel: rel.clone(),
                        line: o.line,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel).then_with(|| a.line.cmp(&b.line)));
    (out, scanned_bytes)
}

/// Per-name occurrence tallies (defs, calls, refs) across `paths`, reusing the
/// same memo + parallel parse as [`find`]. Powers `dead_code`'s "zero references"
/// check. The second tuple element is bytes scanned for the savings counter.
pub fn occurrence_counts(
    root: &Path,
    paths: &[String],
) -> (BTreeMap<String, (usize, usize, usize)>, u64) {
    // 1. Candidate files (same gate as `find`).
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
            if !refs_supported(lang) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            scanned_bytes += meta.len();
            candidates.push((path.to_path_buf(), lang, cache::mtime_ns(&meta)));
        }
    }

    // 2. Warm/cold split against the process memo.
    let cell = OCC_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, lang, mt) in &candidates {
            let key = path.to_string_lossy();
            match store.get(key.as_ref()) {
                Some((cached_mt, _)) if cached_mt == mt => {}
                _ => cold.push((path.clone(), *lang, *mt)),
            }
        }
    }

    // 3. Parse cold files in parallel, then publish.
    let fresh = parallel_extract(&cold);
    {
        let mut store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, mt, occ) in fresh {
            store.insert(path.to_string_lossy().into_owned(), (mt, occ));
        }
    }

    // 4. Tally every occurrence by name (excluding the def itself from call/ref
    //    counts when checking dead code - callers use calls+refs only).
    let mut counts: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    {
        let store = cell.lock().unwrap_or_else(|e| e.into_inner());
        for (path, _lang, mt) in &candidates {
            let key = path.to_string_lossy();
            if let Some((cached_mt, occ)) = store.get(key.as_ref()) {
                if cached_mt != mt {
                    continue;
                }
                for o in occ {
                    let e = counts.entry(o.name.clone()).or_insert((0, 0, 0));
                    match o.role {
                        Role::Def => e.0 += 1,
                        Role::Call => e.1 += 1,
                        Role::Ref => e.2 += 1,
                    }
                }
            }
        }
    }
    (counts, scanned_bytes)
}

/// Languages `symbol_refs` classifies. Daslang is intentionally excluded (its
/// grammar is brace-only and its scripts are mapped by `repo_map`).
fn refs_supported(lang: Lang) -> bool {
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

/// Parse and distill the cold files into occurrence lists, in parallel. Each
/// thread owns a `Parser` (tree-sitter parsers are not `Sync`). Return order is
/// irrelevant - callers key results by path.
fn parallel_extract(cold: &[(PathBuf, Lang, u64)]) -> Vec<(PathBuf, u64, Vec<Occ>)> {
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

fn extract_one(parser: &mut Parser, lang: Lang, path: &Path) -> Vec<Occ> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    extract_occ(parser, lang, &src)
}

/// Distill a source file into every classified name occurrence, symbol-agnostic.
/// The shared parser is retargeted to the file's grammar on each call; Svelte is
/// reduced to its `<script>` logic first (newlines kept, so lines still map).
fn extract_occ(parser: &mut Parser, lang: Lang, src: &str) -> Vec<Occ> {
    let Some(ts) = lang.ts_language() else {
        return Vec::new();
    };
    if parser.set_language(&ts).is_err() {
        return Vec::new();
    }
    let prepared = lang.preprocess(src);
    let Some(tree) = parser.parse(prepared.as_ref(), None) else {
        return Vec::new();
    };
    let bytes = prepared.as_bytes();
    let mut out = Vec::new();
    match lang {
        Lang::Rust => scan_rust(tree.root_node(), bytes, &mut out),
        Lang::Python => scan_python(tree.root_node(), bytes, &mut out),
        Lang::Ts | Lang::Tsx | Lang::Svelte => scan_ts(tree.root_node(), bytes, &mut out),
        Lang::CSharp => scan_csharp(tree.root_node(), bytes, &mut out),
        // C/C++ and GLSL (a tree-sitter-c fork) share the C node kinds.
        _ => scan_cpp(tree.root_node(), bytes, &mut out),
    }
    out
}

/// Record every C/C++/GLSL name occurrence with its classified role.
fn scan_cpp(node: Node, bytes: &[u8], out: &mut Vec<Occ>) {
    if NAME_KINDS_CPP.contains(&node.kind()) {
        let name = text(node, bytes);
        if !name.is_empty() {
            out.push(Occ {
                name: name.to_string(),
                role: classify(node),
                line: node.start_position().row + 1,
            });
        }
    } else if let Some(name) = crate::symbol_def::flecs_tracy_system_name(node, bytes) {
        out.push(Occ {
            name,
            role: Role::Def,
            line: node.start_position().row + 1,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_cpp(child, bytes, out);
    }
}

/// Record every Rust name occurrence with its classified role.
fn scan_rust(node: Node, bytes: &[u8], out: &mut Vec<Occ>) {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "scoped_identifier"
    ) {
        let name = text(node, bytes);
        if !name.is_empty() {
            out.push(Occ {
                name: name.to_string(),
                role: classify_rust(node),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_rust(child, bytes, out);
    }
}

/// Record every Python name occurrence with its classified role.
fn scan_python(node: Node, bytes: &[u8], out: &mut Vec<Occ>) {
    if node.kind() == "identifier" {
        let name = text(node, bytes);
        if !name.is_empty() {
            out.push(Occ {
                name: name.to_string(),
                role: classify_python(node),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_python(child, bytes, out);
    }
}

/// Record every TypeScript/TSX/Svelte name occurrence with its classified role.
/// One classifier serves all three: they share the TypeScript node kinds (TSX
/// adds JSX, Svelte is reduced to its `<script>` by `Lang::preprocess`).
fn scan_ts(node: Node, bytes: &[u8], out: &mut Vec<Occ>) {
    if matches!(
        node.kind(),
        "identifier" | "property_identifier" | "type_identifier"
    ) {
        let name = text(node, bytes);
        if !name.is_empty() {
            out.push(Occ {
                name: name.to_string(),
                role: classify_ts(node),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_ts(child, bytes, out);
    }
}

/// Classify a TypeScript name occurrence. Functions/classes/interfaces/type
/// aliases/enums (incl. `const f = () => {}`) are defs; the function of a call
/// (free `f()` or member `o.f()`) is a call; everything else is a reference.
fn classify_ts(node: Node) -> Role {
    if let Some(p) = node.parent() {
        let pk = p.kind();
        // Named type/function declarations.
        if matches!(
            pk,
            "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "function_declaration"
                | "generator_function_declaration"
                | "function_signature"
                | "method_definition"
                | "abstract_method_signature"
        ) && p.child_by_field_name("name") == Some(node)
        {
            return Role::Def;
        }
        // `const f = () => {}` / `field = function () {}`: a binding whose value
        // is a function counts as a definition of that name.
        if matches!(pk, "variable_declarator" | "public_field_definition")
            && p.child_by_field_name("name") == Some(node)
        {
            if let Some(v) = p.child_by_field_name("value") {
                if matches!(
                    v.kind(),
                    "arrow_function" | "function" | "function_expression"
                ) {
                    return Role::Def;
                }
            }
        }
    }

    // Method call `obj.method(...)`: the property of a member_expression that is
    // itself the function of a call.
    if node.kind() == "property_identifier" {
        if let Some(p) = node.parent() {
            if p.kind() == "member_expression" && p.child_by_field_name("property") == Some(node) {
                if let Some(gp) = p.parent() {
                    if gp.kind() == "call_expression"
                        && gp.child_by_field_name("function") == Some(p)
                    {
                        return Role::Call;
                    }
                }
            }
        }
    }

    // Free call `f(...)`.
    if let Some(p) = node.parent() {
        if p.kind() == "call_expression" && p.child_by_field_name("function") == Some(node) {
            return Role::Call;
        }
    }

    Role::Ref
}

/// Record every C# name occurrence with its classified role.
fn scan_csharp(node: Node, bytes: &[u8], out: &mut Vec<Occ>) {
    if matches!(node.kind(), "identifier" | "generic_name") {
        let name = if node.kind() == "generic_name" {
            node.child_by_field_name("name")
                .map(|n| text(n, bytes))
                .unwrap_or("")
        } else {
            text(node, bytes)
        };
        if !name.is_empty() {
            out.push(Occ {
                name: name.to_string(),
                role: classify_csharp(node),
                line: node.start_position().row + 1,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_csharp(child, bytes, out);
    }
}

/// Classify a C# name occurrence: type/method/property declarations are defs;
/// the function of an `invocation_expression` is a call.
fn classify_csharp(node: Node) -> Role {
    if let Some(p) = node.parent() {
        let pk = p.kind();
        if matches!(
            pk,
            "class_declaration"
                | "struct_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "record_struct_declaration"
                | "method_declaration"
                | "constructor_declaration"
                | "destructor_declaration"
                | "property_declaration"
                | "indexer_declaration"
                | "delegate_declaration"
                | "namespace_declaration"
                | "file_scoped_namespace_declaration"
        ) && p.child_by_field_name("name") == Some(node)
        {
            return Role::Def;
        }
    }

    if let Some(p) = node.parent() {
        if p.kind() == "member_access_expression" && p.child_by_field_name("name") == Some(node) {
            if let Some(gp) = p.parent() {
                if gp.kind() == "invocation_expression"
                    && gp.child_by_field_name("function") == Some(p)
                {
                    return Role::Call;
                }
            }
        }
    }

    if let Some(p) = node.parent() {
        if p.kind() == "invocation_expression" && p.child_by_field_name("function") == Some(node) {
            return Role::Call;
        }
    }

    Role::Ref
}

/// Classify an occurrence as a definition, call, or plain reference. Climbs past
/// `A::b` qualifiers and `f<T>` template wrappers so the construct that actually
/// *uses* the name (declarator, call, type specifier) is the one inspected. This
/// makes out-of-line member definitions (`void Foo::Bar() {}`) and static calls
/// (`Foo::bar()`) classify correctly instead of falling through to a plain ref.
fn classify(node: Node) -> Role {
    // Type definition: the name of a class/struct/union/enum specifier.
    if node.kind() == "type_identifier" {
        if let Some(parent) = node.parent() {
            if matches!(
                parent.kind(),
                "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
            ) && parent.child_by_field_name("name") == Some(node)
            {
                return Role::Def;
            }
        }
    }

    // Method call: `obj.method(...)` / `ptr->method(...)` - the field of a
    // field_expression that is itself the function of a call. Checked before the
    // qualifier climb because field_expression is not a name wrapper.
    if let Some(parent) = node.parent() {
        if parent.kind() == "field_expression" && parent.child_by_field_name("field") == Some(node)
        {
            if let Some(gp) = parent.parent() {
                if gp.kind() == "call_expression"
                    && gp.child_by_field_name("function") == Some(parent)
                {
                    return Role::Call;
                }
            }
        }
    }

    // Climb qualifier/template wrappers so the enclosing declarator/call is seen.
    let mut name = node;
    let mut parent = node.parent();
    while let Some(p) = parent {
        let wraps = matches!(
            p.kind(),
            "qualified_identifier" | "template_function" | "template_type"
        ) && p.child_by_field_name("name") == Some(name);
        if wraps {
            name = p;
            parent = p.parent();
        } else {
            break;
        }
    }
    let Some(parent) = parent else {
        return Role::Ref;
    };

    // Function definition/declaration (incl. out-of-line `void Foo::Bar() {}`).
    if parent.kind() == "function_declarator"
        && parent.child_by_field_name("declarator") == Some(name)
    {
        return Role::Def;
    }

    // Call site: free `foo()`, static `Foo::bar()`, or templated `foo<T>()`.
    if parent.kind() == "call_expression" && parent.child_by_field_name("function") == Some(name) {
        return Role::Call;
    }

    Role::Ref
}

/// Classify a Rust name occurrence. Item names (`fn`, `struct`, `enum`, `trait`,
/// `type`, `union`, `const`, `static`) are defs; the function of a call (incl.
/// `path::seg`, `f::<T>`) and method fields of a method call are calls;
/// everything else is a reference.
fn classify_rust(node: Node) -> Role {
    // Type-like item definitions.
    if node.kind() == "type_identifier" {
        if let Some(p) = node.parent() {
            if matches!(
                p.kind(),
                "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item"
            ) && p.child_by_field_name("name") == Some(node)
            {
                return Role::Def;
            }
        }
    }
    // Function / const / static definition names are `identifier` children.
    if node.kind() == "identifier" {
        if let Some(p) = node.parent() {
            if matches!(p.kind(), "function_item" | "const_item" | "static_item")
                && p.child_by_field_name("name") == Some(node)
            {
                return Role::Def;
            }
        }
    }
    // Method call: `recv.method(...)`.
    if node.kind() == "field_identifier" {
        if let Some(p) = node.parent() {
            if p.kind() == "field_expression" && p.child_by_field_name("field") == Some(node) {
                if let Some(gp) = p.parent() {
                    if gp.kind() == "call_expression"
                        && gp.child_by_field_name("function") == Some(p)
                    {
                        return Role::Call;
                    }
                }
            }
        }
    }
    // Climb `path::seg` and `f::<T>` wrappers to the enclosing call.
    let mut name = node;
    let mut parent = node.parent();
    while let Some(p) = parent {
        let wraps = (p.kind() == "scoped_identifier"
            && p.child_by_field_name("name") == Some(name))
            || (p.kind() == "generic_function" && p.child_by_field_name("function") == Some(name));
        if wraps {
            name = p;
            parent = p.parent();
        } else {
            break;
        }
    }
    let Some(parent) = parent else {
        return Role::Ref;
    };
    if parent.kind() == "call_expression" && parent.child_by_field_name("function") == Some(name) {
        return Role::Call;
    }
    if parent.kind() == "macro_invocation" && parent.child_by_field_name("macro") == Some(name) {
        return Role::Call;
    }
    Role::Ref
}

/// Classify a Python name occurrence. `def`/`class` names are defs; the function
/// of a `call` (free `f()` or attribute `o.f()`) is a call; else a reference.
fn classify_python(node: Node) -> Role {
    let Some(p) = node.parent() else {
        return Role::Ref;
    };
    if matches!(p.kind(), "function_definition" | "class_definition")
        && p.child_by_field_name("name") == Some(node)
    {
        return Role::Def;
    }
    if p.kind() == "call" && p.child_by_field_name("function") == Some(node) {
        return Role::Call;
    }
    if p.kind() == "attribute" && p.child_by_field_name("attribute") == Some(node) {
        if let Some(gp) = p.parent() {
            if gp.kind() == "call" && gp.child_by_field_name("function") == Some(p) {
                return Role::Call;
            }
        }
    }
    Role::Ref
}

fn label(r: Role) -> &'static str {
    match r {
        Role::Def => "definitions",
        Role::Call => "calls",
        Role::Ref => "references",
    }
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

/// The source line at 0-based `row`, trimmed and length-capped for display.
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

    /// Parse `src` as `lang` and return the roles of every occurrence of `symbol`.
    fn roles_in(src: &str, symbol: &str, lang: Lang) -> Vec<Role> {
        let mut p = Parser::new();
        extract_occ(&mut p, lang, src)
            .into_iter()
            .filter(|o| o.name == symbol)
            .map(|o| o.role)
            .collect()
    }

    /// C/C++ shorthand for `roles_in`.
    fn roles(src: &str, symbol: &str) -> Vec<Role> {
        roles_in(src, symbol, Lang::Cpp)
    }

    #[test]
    fn record_definition_is_def() {
        assert_eq!(
            roles("struct Widget { int x; };\n", "Widget"),
            vec![Role::Def]
        );
    }

    #[test]
    fn out_of_line_member_def_is_def_not_ref() {
        // The qualified declarator `Foo::Bar` used to misclassify as a ref.
        let src = "struct Foo { void Bar(); };\nvoid Foo::Bar() { return; }\n";
        let rs = roles(src, "Bar");
        assert!(rs.contains(&Role::Def), "decl in struct: {rs:?}");
        // Two defs: in-class declaration + out-of-line definition.
        assert_eq!(rs.iter().filter(|r| **r == Role::Def).count(), 2, "{rs:?}");
        assert!(
            !rs.contains(&Role::Ref),
            "out-of-line def leaked a ref: {rs:?}"
        );
    }

    #[test]
    fn static_qualified_call_is_call() {
        let src = "void use() { Foo::bar(); }\n";
        assert_eq!(roles(src, "bar"), vec![Role::Call]);
    }

    #[test]
    fn qualified_query_matches_whole_name() {
        let src = "void use() { Foo::bar(); }\n";
        // Querying the whole `Foo::bar` resolves the qualified_identifier as a call.
        assert_eq!(roles(src, "Foo::bar"), vec![Role::Call]);
    }

    #[test]
    fn method_call_is_call() {
        let src = "void use(Obj& o) { o.method(); }\n";
        assert_eq!(roles(src, "method"), vec![Role::Call]);
    }

    #[test]
    fn templated_call_is_call() {
        let src = "void use() { make<int>(); }\n";
        assert_eq!(roles(src, "make"), vec![Role::Call]);
    }

    #[test]
    fn base_class_is_reference() {
        let src = "struct Base {};\nstruct Derived : public Base {};\n";
        let rs = roles(src, "Base");
        assert!(rs.contains(&Role::Def), "Base's own def: {rs:?}");
        assert!(rs.contains(&Role::Ref), "inheritance ref: {rs:?}");
        assert!(!rs.contains(&Role::Call), "{rs:?}");
    }

    #[test]
    fn rust_fn_def_and_call_classify() {
        let src = "fn helper() {}\nfn run() { helper(); }\n";
        let rs = roles_in(src, "helper", Lang::Rust);
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert!(rs.contains(&Role::Call), "{rs:?}");
    }

    #[test]
    fn rust_struct_is_def_and_method_is_call() {
        assert_eq!(
            roles_in("struct Widget { x: i32 }\n", "Widget", Lang::Rust),
            vec![Role::Def]
        );
        assert_eq!(
            roles_in("fn use_it(o: Obj) { o.method(); }\n", "method", Lang::Rust),
            vec![Role::Call]
        );
    }

    #[test]
    fn rust_const_and_static_are_defs() {
        assert_eq!(
            roles_in(
                "pub const INPUT_SPIN: u8 = 1 << 5;\n",
                "INPUT_SPIN",
                Lang::Rust
            ),
            vec![Role::Def]
        );
        let rs = roles_in(
            "pub static MAX: i32 = 1;\nfn use_it() { let _ = MAX; }\n",
            "MAX",
            Lang::Rust,
        );
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert!(rs.contains(&Role::Ref), "{rs:?}");
    }

    #[test]
    fn flecs_system_and_tracy_zone_strings_are_defs() {
        let src = r#"
void Register() {
  ecs.system("LightCollect")
      .run([](flecs::iter&) {
          ZoneScopedN("LightCollect");
      });
  puts("LightCollect");
}
"#;
        let rs = roles_in(src, "LightCollect", Lang::Cpp);
        assert!(
            rs.iter().filter(|r| **r == Role::Def).count() >= 2,
            "ecs.system + ZoneScopedN should be defs: {rs:?}"
        );
        // Ordinary string content must stay invisible.
        assert_eq!(
            roles_in(r#"void f() { puts("OtherName"); }"#, "OtherName", Lang::Cpp),
            Vec::<Role>::new()
        );
    }

    #[test]
    fn python_def_call_and_method_classify() {
        let src = "def helper():\n    pass\n\ndef run():\n    helper()\n";
        let rs = roles_in(src, "helper", Lang::Python);
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert!(rs.contains(&Role::Call), "{rs:?}");
        assert_eq!(
            roles_in("def f(o):\n    o.method()\n", "method", Lang::Python),
            vec![Role::Call]
        );
    }

    #[test]
    fn ts_fn_class_and_calls_classify() {
        // function declaration + free call
        let rs = roles_in(
            "function helper() {}\nfunction run() { helper(); }\n",
            "helper",
            Lang::Ts,
        );
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert!(rs.contains(&Role::Call), "{rs:?}");
        // class + interface + type alias are defs
        assert_eq!(
            roles_in("class Widget {}\n", "Widget", Lang::Ts),
            vec![Role::Def]
        );
        assert_eq!(
            roles_in("interface Shape {}\n", "Shape", Lang::Ts),
            vec![Role::Def]
        );
        // arrow const is a def; method call is a call
        assert_eq!(
            roles_in("const make = () => 1;\n", "make", Lang::Ts),
            vec![Role::Def]
        );
        assert_eq!(
            roles_in("function f(o: any) { o.poke(); }\n", "poke", Lang::Ts),
            vec![Role::Call]
        );
    }

    #[test]
    fn csharp_def_and_member_call() {
        let src = "namespace N {\n  class Plugin {\n    void Awake() { Logger.LogInfo(\"hi\"); }\n  }\n}\n";
        let rs = roles_in(src, "Awake", Lang::CSharp);
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert_eq!(roles_in(src, "LogInfo", Lang::CSharp), vec![Role::Call]);
    }

    #[test]
    fn svelte_extracts_script_logic() {
        let src = "<script lang=\"ts\">\nfunction onClick() {}\nonClick();\n</script>\n<button on:click={onClick}>go</button>\n";
        let rs = roles_in(src, "onClick", Lang::Svelte);
        assert!(rs.contains(&Role::Def), "{rs:?}");
        assert!(rs.contains(&Role::Call), "{rs:?}");
    }

    #[test]
    fn full_output_summarizes_and_filters() {
        let dir = std::env::temp_dir().join(format!("cbtest_refs_{}", std::process::id()));
        let srcdir = dir.join("src");
        std::fs::create_dir_all(&srcdir).unwrap();
        std::fs::write(
            srcdir.join("a.cpp"),
            "struct Foo { void Bar(); };\nvoid Foo::Bar() {}\nvoid use() { Foo f; f.Bar(); }\n",
        )
        .unwrap();
        let all = find(
            &dir,
            &serde_json::json!({"symbol": "Bar", "paths": ["src"]}),
        )
        .unwrap();
        assert!(all.contains("def"), "{all}");
        assert!(all.contains("call"), "{all}");
        // Definition-only fast path emits the marker and no call section.
        let defs = find(
            &dir,
            &serde_json::json!({"symbol": "Bar", "paths": ["src"], "kind": "def"}),
        )
        .unwrap();
        assert!(defs.contains("definition-only"), "{defs}");
        assert!(!defs.contains("calls:"), "{defs}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
