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

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

use crate::cache;
use crate::lang::Lang;
use crate::store::{
    EdgeInput, Store, SymbolInput, CONFIDENCE_NAME_MATCH, CONFIDENCE_TREESITTER,
    EDGE_SOURCE_NAME_MATCH, EDGE_SOURCE_SCIP, EDGE_SOURCE_TREESITTER, STAGE_CALL_GRAPH,
};
use crate::{stats, walk};

/// Severity tier for a function's fan-in / k-core blast radius.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HubTier {
    #[default]
    None,
    Hub,
    CoreHub,
}

/// Lightweight fan-in and k-core structural summary for a symbol.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubInfo {
    pub fan_in: usize,
    pub prod_fan_in: usize,
    pub test_fan_in: usize,
    pub fan_out: usize,
    pub k_core: usize,
    pub tier: HubTier,
}

impl HubInfo {
    /// Compact badge rendered when a symbol is a notable hub or core-hub.
    pub fn badge(&self) -> Option<String> {
        match self.tier {
            HubTier::CoreHub => Some(format!(
                "[core-hub fan-in={} k-core={}]",
                self.fan_in, self.k_core
            )),
            HubTier::Hub => Some(format!(
                "[hub fan-in={} k-core={}]",
                self.fan_in, self.k_core
            )),
            HubTier::None => None,
        }
    }
}

/// Classify a symbol's fan-in and k-core into a [`HubInfo`] badge.
#[allow(dead_code)] // Used by unit tests + hub badge helpers; keep public for triage.
pub fn classify_hub(
    fan_in: usize,
    prod_fan_in: usize,
    test_fan_in: usize,
    fan_out: usize,
    k_core: usize,
) -> HubInfo {
    let tier = if prod_fan_in >= 10 || fan_in >= 12 || k_core >= 4 {
        HubTier::CoreHub
    } else if prod_fan_in >= 5 || fan_in >= 5 || k_core >= 3 {
        HubTier::Hub
    } else {
        HubTier::None
    };
    HubInfo {
        fan_in,
        prod_fan_in,
        test_fan_in,
        fan_out,
        k_core,
        tier,
    }
}

/// A ranked caller call-site carrying provenance and test-fixture classification.
#[derive(Debug, Clone, PartialEq)]
pub struct CallerSite {
    pub name: String,
    pub rel: String,
    pub line: usize,
    pub text: String,
    pub source: String,
    pub confidence: f64,
    pub is_test: bool,
}

/// True when `rel` lives under a test/fixture/mock directory or matches common
/// test-file naming conventions across C++, Rust, Python, TS/JS, and C#.
pub fn is_test_path(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    for dir in &parts[..parts.len().saturating_sub(1)] {
        let d = dir.to_ascii_lowercase();
        if matches!(
            d.as_str(),
            "tests"
                | "test"
                | "__tests__"
                | "spec"
                | "specs"
                | "fixtures"
                | "fixture"
                | "mocks"
                | "mock"
                | "testdata"
        ) {
            return true;
        }
    }
    let file = *parts.last().unwrap_or(&"");
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    let lower = stem.to_ascii_lowercase();
    if matches!(lower.as_str(), "test" | "tests")
        || lower.starts_with("test_")
        || lower.starts_with("tests_")
        || lower.starts_with("fixture_")
        || lower.starts_with("mock_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
        || lower.ends_with(".test")
        || lower.ends_with(".spec")
        || lower.ends_with("_spec")
        || lower.ends_with("_fixture")
        || lower.ends_with("_fixtures")
        || lower.ends_with("_mock")
        || lower.ends_with("_mocks")
    {
        return true;
    }
    // CamelCase C#/Java test files (e.g. WidgetTests.cs, SolverTest.cs) without
    // false-matching words like `latest.rs` or `contest.cpp`.
    if (stem.ends_with("Tests") && stem.len() > 5) || (stem.ends_with("Test") && stem.len() > 4) {
        return true;
    }
    false
}

/// True when a caller belongs to a test file, fixture, or test symbol/module.
pub fn is_test_caller(name: &str, rel: &str) -> bool {
    if is_test_path(rel) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("::tests::")
        || lower.contains("::test::")
        || lower.starts_with("tests::")
        || lower.starts_with("test::")
    {
        return true;
    }
    let tail = seg(name);
    let tail_l = tail.to_ascii_lowercase();
    tail_l.starts_with("test_")
        || tail_l.starts_with("mock_")
        || tail_l.starts_with("fixture_")
        || tail_l.starts_with("stub_")
        || tail.starts_with("TEST_CASE")
        || tail.starts_with("SCENARIO")
}

/// Sort callers so production callers always precede test/fixture callers,
/// then highest provenance confidence first (`scip` 1.0 > `treesitter` 0.5 > `name-match` 0.3),
/// with deterministic lexical tie-breaking.
pub fn rank_callers(callers: &mut [CallerSite]) {
    callers.sort_by(|a, b| {
        a.is_test
            .cmp(&b.is_test)
            .then_with(|| b.confidence.total_cmp(&a.confidence))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.rel.cmp(&b.rel))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// Format caller count header, preserving true total count while breaking out
/// production vs test/fixture counts when any test callers exist.
pub fn format_caller_count(total: usize, prod: usize, test: usize) -> String {
    if test == 0 {
        format!("{total}")
    } else {
        format!("{total} total: {prod} prod, {test} test")
    }
}

/// Format provenance tag for an edge (e.g. `scip 1.0`, `treesitter 0.5`).
pub fn format_provenance(source: &str, confidence: f64) -> String {
    format!("{source} {confidence:.1}")
}

/// Format inline bracket tag for a caller row.
pub fn format_caller_tag(c: &CallerSite) -> String {
    if c.is_test {
        format!("[test, {} {:.1}]", c.source, c.confidence)
    } else {
        format!("[{} {:.1}]", c.source, c.confidence)
    }
}

/// Linear-time Batagelj-Zaversnik k-core decomposition over the undirected
/// projection of the call graph. Returns exact coreness per symbol segment.
pub fn compute_k_cores(
    callees: &BTreeMap<String, BTreeSet<String>>,
    callers: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, usize> {
    let mut nodes: Vec<String> = Vec::new();
    let mut idx_of: HashMap<String, usize> = HashMap::new();
    let mut intern = |name: &str| -> usize {
        if let Some(&id) = idx_of.get(name) {
            id
        } else {
            let id = nodes.len();
            nodes.push(name.to_string());
            idx_of.insert(name.to_string(), id);
            id
        }
    };

    for (u, vs) in callees {
        let _ = intern(u);
        for v in vs {
            let _ = intern(v);
        }
    }
    for (v, us) in callers {
        let _ = intern(v);
        for u in us {
            let _ = intern(u);
        }
    }

    let n = nodes.len();
    if n == 0 {
        return BTreeMap::new();
    }

    let mut adj: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (u, vs) in callees {
        let ui = idx_of[u];
        for v in vs {
            let vi = idx_of[v];
            if ui != vi {
                adj[ui].insert(vi);
                adj[vi].insert(ui);
            }
        }
    }
    for (v, us) in callers {
        let vi = idx_of[v];
        for u in us {
            let ui = idx_of[u];
            if ui != vi {
                adj[ui].insert(vi);
                adj[vi].insert(ui);
            }
        }
    }

    let mut deg: Vec<usize> = adj.iter().map(|s| s.len()).collect();
    let max_deg = deg.iter().copied().max().unwrap_or(0);
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); max_deg + 1];
    for (u, &d) in deg.iter().enumerate() {
        buckets[d].push(u);
    }

    let mut peeled = vec![false; n];
    let mut core = vec![0usize; n];
    let mut cur_d = 0usize;

    while cur_d <= max_deg {
        let Some(u) = buckets[cur_d].pop() else {
            cur_d += 1;
            continue;
        };
        if peeled[u] || deg[u] != cur_d {
            continue;
        }
        peeled[u] = true;
        core[u] = cur_d;
        for &v in &adj[u] {
            if !peeled[v] && deg[v] > cur_d {
                deg[v] -= 1;
                buckets[deg[v]].push(v);
            }
        }
    }

    let mut out = BTreeMap::new();
    for (i, name) in nodes.into_iter().enumerate() {
        out.insert(name, core[i]);
    }
    out
}

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
    source: String,
    confidence: f64,
}

/// Everything `call_graph` needs from one file, independent of the query symbol.
#[derive(Default)]
struct Distilled {
    defs: Vec<DefSite>,
    edges: Vec<Edge>,
}

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
        hub,
        scanned_bytes,
        bytes_read,
        cache_hits,
        cache_misses,
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
        &hub,
        max,
        budget,
    );
    stats::record_ext(
        "call_graph",
        stats::RecordMeta {
            baseline: scanned_bytes / 4,
            returned: (out.len() / 4) as u64,
            bytes_read,
            cache_hits,
            cache_misses,
            reason: None,
        },
    );
    Ok(out)
}

/// The query-filtered hop in `call_graph`'s native (private) shapes, ready for
/// `render`. Factored out of `build` so other tools can reuse the same walk.
struct Hop {
    defs: Vec<Site>,
    callees: BTreeMap<String, (String, f64)>,
    callers: Vec<CallerSite>,
    hub: HubInfo,
    scanned_bytes: u64,
    bytes_read: u64,
    cache_hits: u64,
    cache_misses: u64,
}

fn match_edge_name(
    edge_name: &str,
    query: &str,
    edge_source: &str,
    edge_conf: f64,
) -> Option<(String, f64)> {
    if edge_name == query {
        return Some((edge_source.to_string(), edge_conf));
    }
    if edge_name.ends_with(&format!("::{query}")) || query.ends_with(&format!("::{edge_name}")) {
        return Some((edge_source.to_string(), edge_conf));
    }
    if seg(edge_name) != seg(query) {
        return None;
    }
    let edge_qual = edge_name.contains("::");
    let query_qual = query.contains("::");
    if edge_qual && query_qual {
        if edge_source == EDGE_SOURCE_SCIP {
            return None;
        }
        return Some((EDGE_SOURCE_NAME_MATCH.to_string(), CONFIDENCE_NAME_MATCH));
    }
    Some((edge_source.to_string(), edge_conf))
}

fn rel_under_paths(rel: &str, paths: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    for p in paths {
        let norm = p.trim().trim_matches('/');
        if norm.is_empty() || norm == "." {
            return true;
        }
        if rel == norm || rel.starts_with(&format!("{norm}/")) {
            return true;
        }
    }
    false
}

/// Merge a caller candidate into `list`, collapsing duplicate edges from the
/// same file/function (e.g. a SCIP edge upgrading a tree-sitter edge) while
/// keeping the higher-confidence provenance.
fn merge_caller(list: &mut Vec<CallerSite>, cand: CallerSite) {
    let cand_seg = seg(&cand.name);
    let cand_qual = cand.name.contains("::");
    if let Some(existing) = list.iter_mut().find(|e| {
        if e.rel != cand.rel {
            return false;
        }
        if e.name == cand.name {
            return true;
        }
        if seg(&e.name) == cand_seg {
            let ex_qual = e.name.contains("::");
            return e.line == cand.line || !ex_qual || !cand_qual;
        }
        false
    }) {
        let upgrade = cand.confidence > existing.confidence
            || ((cand.confidence - existing.confidence).abs() < 1e-9
                && cand.name.len() > existing.name.len());
        if upgrade {
            *existing = cand;
        }
    } else {
        list.push(cand);
    }
}

/// Merge a callee candidate into `map`, collapsing bare vs qualified duplicates
/// (e.g. `helper` from tree-sitter upgraded by `demo::helper` from SCIP).
fn merge_callee(
    map: &mut BTreeMap<String, (String, f64)>,
    callee: &str,
    source: String,
    confidence: f64,
) {
    let cseg = seg(callee);
    let cqual = callee.contains("::");
    let existing_key = map
        .keys()
        .find(|k| {
            *k == callee || (seg(k) == cseg && (!k.contains("::") || !cqual))
        })
        .cloned();
    if let Some(k) = existing_key {
        let (_, ex_conf) = map[&k];
        let upgrade = confidence > ex_conf
            || ((confidence - ex_conf).abs() < 1e-9 && callee.len() > k.len());
        if upgrade {
            map.remove(&k);
            map.insert(callee.to_string(), (source, confidence));
        }
    } else {
        map.insert(callee.to_string(), (source, confidence));
    }
}

/// Walk `paths`, distill every file (warm via the disk cache, cold in parallel),
/// and filter the graphs down to `symbol`'s defs, callees, ranked callers, and
/// hub/k-core metrics.
fn gather(
    root: &Path,
    symbol: &str,
    paths: &[String],
    want_callers: bool,
    want_callees: bool,
) -> Hop {
    // Steps 1–4: sync SCIP overlay, enumerate, warm/cold split, parse, gather distilled graphs.
    let (per_file, scanned_bytes, bytes_read, cache_hits, cache_misses) =
        distilled_files(root, paths);

    // 5. Filter the distilled graphs by the query symbol and build graph edges for k-core.
    let mut defs: Vec<Site> = Vec::new();
    let mut callees: BTreeMap<String, (String, f64)> = BTreeMap::new();
    let mut callers: Vec<CallerSite> = Vec::new();
    let mut adj_callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut adj_callers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

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
            let cr_seg = seg(&e.caller).to_string();
            let ce_seg = seg(&e.callee).to_string();
            if cr_seg != ce_seg {
                adj_callees
                    .entry(cr_seg.clone())
                    .or_default()
                    .insert(ce_seg.clone());
                adj_callers.entry(ce_seg).or_default().insert(cr_seg);
            }

            if want_callers && !name_matches(&e.caller, symbol) {
                if let Some((src, conf)) =
                    match_edge_name(&e.callee, symbol, &e.source, e.confidence)
                {
                    let is_test = is_test_caller(&e.caller, rel);
                    merge_caller(
                        &mut callers,
                        CallerSite {
                            name: e.caller.clone(),
                            rel: rel.clone(),
                            line: e.line,
                            text: e.text.clone(),
                            source: src,
                            confidence: conf,
                            is_test,
                        },
                    );
                }
            }
            if want_callees && !name_matches(&e.callee, symbol) {
                if let Some((src, conf)) =
                    match_edge_name(&e.caller, symbol, &e.source, e.confidence)
                {
                    merge_callee(&mut callees, &e.callee, src, conf);
                }
            }
        }
    }

    rank_callers(&mut callers);
    let prod_fan_in = callers.iter().filter(|c| !c.is_test).count();
    let test_fan_in = callers.len().saturating_sub(prod_fan_in);
    let k_cores = compute_k_cores(&adj_callees, &adj_callers);
    let k_core = k_cores.get(seg(symbol)).copied().unwrap_or(0);
    let hub = classify_hub(
        callers.len(),
        prod_fan_in,
        test_fan_in,
        callees.len(),
        k_core,
    );

    Hop {
        defs,
        callees,
        callers,
        hub,
        scanned_bytes,
        bytes_read,
        cache_hits,
        cache_misses,
    }
}

/// Steps 1–4 of the walk shared by [`gather`] and [`adjacency`]: sync any
/// present `.scip` overlay, enumerate candidate files, warm/cold split against
/// the per-workspace SQLite store, parse cold files in parallel (upserting
/// `files`, `symbols`, and confidence-tagged `edges`), merge SCIP edges, and
/// return every file's distilled graph (sorted by rel for deterministic
/// tie-breaks) plus `(scanned_bytes, bytes_read, cache_hits, cache_misses)`.
fn distilled_files(
    root: &Path,
    paths: &[String],
) -> (Vec<(String, Distilled)>, u64, u64, u64, u64) {
    // 0. Opportunistic SCIP sync (fingerprint-cached; silent fallback when absent).
    let _ = crate::scip::sync_if_present(root);

    // 1. Candidate files (path, rel, language, mtime, size). Daslang is out of
    //    scope here; `repo_map` maps its declarations instead.
    let prune = walk::prune_set(root);
    let mut candidates: Vec<(PathBuf, String, Lang, u64, u64)> = Vec::new();
    let mut scanned_bytes = 0u64;
    for p in paths {
        let base = root.join(p);
        for path_owned in walk::files(&base, &prune) {
            let path = path_owned.as_path();
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            if !cg_supported(lang) {
                continue;
            }
            let Ok(meta) = path.metadata() else { continue };
            let sz = meta.len();
            scanned_bytes += sz;
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            candidates.push((path.to_path_buf(), rel, lang, cache::mtime_ns(&meta), sz));
        }
    }

    // 2. Warm/cold split against the per-workspace SQLite store.
    let store = Store::open(root).ok();
    let mut warm_ids: HashMap<String, i64> = HashMap::new();
    let mut cold: Vec<(PathBuf, Lang, u64)> = Vec::new();
    let mut cold_meta: HashMap<PathBuf, (String, Lang, u64)> = HashMap::new();
    let mut bytes_read = 0u64;

    for (path, rel, lang, mt, sz) in &candidates {
        let warm = store
            .as_ref()
            .and_then(|st| st.file_stage_mtime(rel, STAGE_CALL_GRAPH).ok().flatten())
            .filter(|&(_, stored_mt)| stored_mt == *mt);
        if let Some((file_id, _)) = warm {
            warm_ids.insert(rel.clone(), file_id);
        } else {
            bytes_read += *sz;
            cold.push((path.clone(), *lang, *mt));
            cold_meta.insert(path.clone(), (rel.clone(), *lang, *sz));
        }
    }
    let cache_hits = warm_ids.len() as u64;
    let cache_misses = cold.len() as u64;

    // 3. Parse cold files in parallel, then batch-upsert into SQLite.
    let fresh = parallel_extract(&cold);
    if !fresh.is_empty() {
        if let Some(st) = store.as_ref() {
            let _ = st.with_immediate_tx(|s| {
                for (path, mt, d) in &fresh {
                    let Some((rel, lang, sz)) = cold_meta.get(path) else {
                        continue;
                    };
                    let file_id = s.upsert_file(rel, *mt, *sz, lang.label())?;
                    s.clear_file_edges_by_source(file_id, EDGE_SOURCE_TREESITTER)?;

                    let mut sym_ids: HashMap<&str, i64> = HashMap::with_capacity(d.defs.len());
                    for ds in &d.defs {
                        let sym_id = s.upsert_symbol(&SymbolInput {
                            file_id,
                            name: &ds.name,
                            qual_name: &ds.name,
                            kind: "function",
                            line: ds.line,
                            end_line: 0,
                            signature: &ds.text,
                            doc: "",
                            exported: false,
                            source: EDGE_SOURCE_TREESITTER,
                        })?;
                        sym_ids.entry(ds.name.as_str()).or_insert(sym_id);
                    }
                    for e in &d.edges {
                        let src_symbol_id = sym_ids.get(e.caller.as_str()).copied();
                        s.upsert_edge(&EdgeInput {
                            file_id,
                            src_symbol_id,
                            src_name: &e.caller,
                            dst_name: &e.callee,
                            kind: "call",
                            line: e.line,
                            site_text: &e.text,
                            source: &e.source,
                            confidence: e.confidence,
                        })?;
                    }
                    s.set_file_stage(file_id, STAGE_CALL_GRAPH, *mt, None)?;
                }
                Ok(())
            });
        }
    }

    // 4. Gather every candidate's distilled graph, preserving provenance/confidence
    //    and merging any SQLite SCIP / name-match overlay edges.
    let mut fresh_by_rel: HashMap<String, Distilled> = HashMap::with_capacity(fresh.len());
    for (path, _mt, d) in fresh {
        if let Some((rel, _, _)) = cold_meta.get(&path) {
            fresh_by_rel.insert(rel.clone(), d);
        }
    }

    let mut by_rel: BTreeMap<String, Distilled> = BTreeMap::new();
    let mut loaded_from_store: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (_path, rel, _lang, _mt, _sz) in candidates {
        if let Some(d) = fresh_by_rel.remove(&rel) {
            by_rel.insert(rel, d);
            continue;
        }
        if let (Some(st), Some(&file_id)) = (store.as_ref(), warm_ids.get(&rel)) {
            let defs = st
                .symbols_for_file_kind(file_id, "function")
                .unwrap_or_default()
                .into_iter()
                .map(|s| DefSite {
                    name: s.name,
                    line: s.line,
                    text: s.signature,
                })
                .collect();
            let edges = st
                .edges_for_file(file_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.kind == "call")
                .map(|e| Edge {
                    caller: e.src_name,
                    callee: e.dst_name,
                    line: e.line,
                    text: e.site_text,
                    source: e.source,
                    confidence: e.confidence,
                })
                .collect();
            loaded_from_store.insert(rel.clone());
            by_rel.insert(rel, Distilled { defs, edges });
        }
    }

    // Merge non-treesitter overlay edges (SCIP 1.0, name-match 0.3) for cold files
    // and any in-scope files present in the SCIP index.
    if let Some(st) = store.as_ref() {
        for overlay_src in [EDGE_SOURCE_SCIP, EDGE_SOURCE_NAME_MATCH] {
            if let Ok(overlay_edges) = st.all_edges_by_source(overlay_src) {
                for e in overlay_edges {
                    if e.kind != "call" || !rel_under_paths(&e.rel_path, paths) {
                        continue;
                    }
                    if loaded_from_store.contains(&e.rel_path) {
                        continue; // already loaded via st.edges_for_file(file_id)
                    }
                    by_rel
                        .entry(e.rel_path)
                        .or_default()
                        .edges
                        .push(Edge {
                            caller: e.src_name,
                            callee: e.dst_name,
                            line: e.line,
                            text: e.site_text,
                            source: e.source,
                            confidence: e.confidence,
                        });
                }
            }
        }
    }

    (
        by_rel.into_iter().collect(),
        scanned_bytes,
        bytes_read,
        cache_hits,
        cache_misses,
    )
}

/// One hop of the call graph in plain owned shapes, for other tools
/// (`symbol_context`, `diff_map`) to compose without touching the private
/// `Site`/`Distilled` types. Always gathers both directions.
pub struct OneHop {
    /// Names of functions the symbol calls.
    pub callees: Vec<String>,
    /// Provenance per callee: `callee_name -> (source, confidence)`.
    pub callee_provenance: BTreeMap<String, (String, f64)>,
    /// Functions that call the symbol: `(caller_name, rel, line)`, ranked prod-first.
    #[allow(dead_code)] // Compat shape for older callers; prefer `caller_sites`.
    pub callers: Vec<(String, String, usize)>,
    /// Rich ranked caller call-sites (prod before test, confidence DESC).
    pub caller_sites: Vec<CallerSite>,
    /// Fan-in / k-core hub summary for the symbol.
    pub hub: HubInfo,
    /// Bytes scanned, for the savings counter.
    pub scanned_bytes: u64,
    /// Bytes actually read from disk (cold files only).
    pub bytes_read: u64,
    /// Warm SQLite cache hits.
    pub cache_hits: u64,
    /// Cold SQLite cache misses.
    pub cache_misses: u64,
}

/// Public entry for `symbol_context`/`diff_map`: callers + callees of `symbol`.
pub fn one_hop(root: &Path, symbol: &str, paths: &[String]) -> OneHop {
    let h = gather(root, symbol, paths, true, true);
    let callees: Vec<String> = h.callees.keys().cloned().collect();
    let callers: Vec<(String, String, usize)> = h
        .callers
        .iter()
        .map(|c| (c.name.clone(), c.rel.clone(), c.line))
        .collect();
    OneHop {
        callees,
        callee_provenance: h.callees,
        callers,
        caller_sites: h.callers,
        hub: h.hub,
        scanned_bytes: h.scanned_bytes,
        bytes_read: h.bytes_read,
        cache_hits: h.cache_hits,
        cache_misses: h.cache_misses,
    }
}

/// Whole-tree call adjacency, built once from the distilled per-file graphs
/// (warm via the disk cache). Names are normalized to their trailing `::`
/// segment so `Foo::Bar` and `Bar` unify, matching the rest of `call_graph`.
/// Powers `call_path`'s BFS, `undocumented`'s fan-in ranking, and `batch_context`.
#[derive(Default)]
pub struct Adjacency {
    /// caller segment → the callee segments it invokes.
    pub callees: BTreeMap<String, BTreeSet<String>>,
    /// callee segment → the caller segments that invoke it.
    pub callers: BTreeMap<String, BTreeSet<String>>,
    /// One representative definition site per segment: name → (rel, line).
    pub sites: BTreeMap<String, (String, usize)>,
    /// Coreness (k-core number) per symbol segment.
    pub k_cores: BTreeMap<String, usize>,
    /// Ranked caller sites per callee segment (prod first, confidence DESC).
    pub caller_sites: BTreeMap<String, Vec<CallerSite>>,
    /// Best provenance per `(caller_seg -> callee_seg)`.
    #[allow(dead_code)] // Reserved for call_path provenance display / SCIP merge.
    pub callee_provenance: BTreeMap<String, BTreeMap<String, (String, f64)>>,
    /// Bytes scanned, for the savings counter.
    pub scanned_bytes: u64,
    /// Bytes actually read from disk (cold files only).
    pub bytes_read: u64,
    /// Warm SQLite cache hits.
    pub cache_hits: u64,
    /// Cold SQLite cache misses.
    pub cache_misses: u64,
}

/// Build the whole-tree call [`Adjacency`] over `paths`.
pub fn adjacency(root: &Path, paths: &[String]) -> Adjacency {
    let (per_file, scanned_bytes, bytes_read, cache_hits, cache_misses) =
        distilled_files(root, paths);
    let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut callers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut sites: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut caller_sites: BTreeMap<String, Vec<CallerSite>> = BTreeMap::new();
    let mut callee_provenance: BTreeMap<String, BTreeMap<String, (String, f64)>> = BTreeMap::new();

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
            callers.entry(ce.clone()).or_default().insert(cr.clone());

            let prov_map = callee_provenance.entry(cr).or_default();
            merge_callee(prov_map, &ce, e.source.clone(), e.confidence);

            let is_test = is_test_caller(&e.caller, rel);
            let list = caller_sites.entry(ce).or_default();
            merge_caller(
                list,
                CallerSite {
                    name: e.caller.clone(),
                    rel: rel.clone(),
                    line: e.line,
                    text: e.text.clone(),
                    source: e.source.clone(),
                    confidence: e.confidence,
                    is_test,
                },
            );
        }
    }

    for list in caller_sites.values_mut() {
        rank_callers(list);
    }
    let k_cores = compute_k_cores(&callees, &callers);

    Adjacency {
        callees,
        callers,
        sites,
        k_cores,
        caller_sites,
        callee_provenance,
        scanned_bytes,
        bytes_read,
        cache_hits,
        cache_misses,
    }
}

impl Adjacency {
    /// Ranked callers of `symbol` with provenance and test classification
    /// (prod callers first, then highest confidence).
    pub fn ranked_callers_of(&self, symbol: &str) -> Vec<CallerSite> {
        let key = seg(symbol);
        if let Some(list) = self.caller_sites.get(key) {
            return list.clone();
        }
        let Some(cs) = self.callers.get(key) else {
            return Vec::new();
        };
        let mut out: Vec<CallerSite> = cs
            .iter()
            .map(|name| {
                let (rel, line) = self
                    .sites
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| ("?".into(), 0));
                let is_test = is_test_caller(name, &rel);
                CallerSite {
                    name: name.clone(),
                    rel,
                    line,
                    text: String::new(),
                    source: EDGE_SOURCE_TREESITTER.to_string(),
                    confidence: CONFIDENCE_TREESITTER,
                    is_test,
                }
            })
            .collect();
        rank_callers(&mut out);
        out
    }

    /// Callers of `symbol` as `(caller_name, rel, line)`, ranked prod-first to match [`one_hop`].
    pub fn callers_of(&self, symbol: &str) -> Vec<(String, String, usize)> {
        self.ranked_callers_of(symbol)
            .into_iter()
            .map(|c| (c.name, c.rel, c.line))
            .collect()
    }

    /// Lightweight fan-in and k-core hub summary for `symbol`.
    pub fn hub_info(&self, symbol: &str) -> HubInfo {
        let key = seg(symbol);
        let callers = self.ranked_callers_of(symbol);
        let prod_fan_in = callers.iter().filter(|c| !c.is_test).count();
        let test_fan_in = callers.len().saturating_sub(prod_fan_in);
        let fan_out = self.callees.get(key).map(|s| s.len()).unwrap_or(0);
        let k_core = self
            .k_cores
            .get(key)
            .copied()
            .unwrap_or_else(|| {
                compute_k_cores(&self.callees, &self.callers)
                    .get(key)
                    .copied()
                    .unwrap_or(0)
            });
        classify_hub(callers.len(), prod_fan_in, test_fan_in, fan_out, k_core)
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
        Lang::Rust => visit_rust(tree.root_node(), bytes, &lines, None, false, &mut d),
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
                source: EDGE_SOURCE_TREESITTER.to_string(),
                confidence: CONFIDENCE_TREESITTER,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_cpp(child, bytes, lines, cur, d);
    }
}

/// True when `node` is immediately preceded by a Rust `#[test]` or `#[cfg(test)]` attribute.
fn rust_has_test_attr(node: Node, bytes: &[u8]) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(sib) = prev {
        match sib.kind() {
            "attribute_item" => {
                let t = text(sib, bytes);
                if t.contains("test") {
                    return true;
                }
                prev = sib.prev_sibling();
            }
            "line_comment" | "block_comment" => {
                prev = sib.prev_sibling();
            }
            _ => break,
        }
    }
    false
}

/// Rust analogue of `visit_cpp`: `function_item` defs, plus `call_expression`
/// and `macro_invocation` edges. Functions inside `#[cfg(test)] mod tests` or
/// annotated with `#[test]` are prefixed with `tests::` so caller ranking
/// distinguishes inline Rust unit tests from production callers.
fn visit_rust(
    node: Node,
    bytes: &[u8],
    lines: &[&str],
    current: Option<&str>,
    in_test_mod: bool,
    d: &mut Distilled,
) {
    let mut cur = current;
    let mut owned: Option<String> = None;
    let mut next_test_mod = in_test_mod;

    if node.kind() == "mod_item" {
        let mod_name = node
            .child_by_field_name("name")
            .map(|n| text(n, bytes))
            .unwrap_or("");
        if matches!(mod_name, "tests" | "test") || rust_has_test_attr(node, bytes) {
            next_test_mod = true;
        }
    }

    if node.kind() == "function_item" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let raw_name = text(name_node, bytes);
            let is_test_fn = next_test_mod || rust_has_test_attr(node, bytes);
            let name = if is_test_fn && !raw_name.starts_with("tests::") {
                format!("tests::{raw_name}")
            } else {
                raw_name.to_string()
            };
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
            source: EDGE_SOURCE_TREESITTER.to_string(),
            confidence: CONFIDENCE_TREESITTER,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_rust(child, bytes, lines, cur, next_test_mod, d);
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
                source: EDGE_SOURCE_TREESITTER.to_string(),
                confidence: CONFIDENCE_TREESITTER,
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
                source: EDGE_SOURCE_TREESITTER.to_string(),
                confidence: CONFIDENCE_TREESITTER,
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
                source: EDGE_SOURCE_TREESITTER.to_string(),
                confidence: CONFIDENCE_TREESITTER,
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
    callees: &BTreeMap<String, (String, f64)>,
    callers: &[CallerSite],
    hub: &HubInfo,
    max: usize,
    budget: usize,
) -> String {
    let mut out = format!("call_graph - \"{symbol}\"  (roots {paths:?}, direction {direction})");
    if let Some(badge) = hub.badge() {
        out.push_str(&format!("  {badge}"));
    }
    out.push('\n');
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
            for (i, (c, (src, conf))) in callees.iter().enumerate() {
                if i >= max {
                    out.push_str(&format!("  … (+{} more)\n", callees.len() - max));
                    break;
                }
                out.push_str(&format!("  {c}  [{}]\n", format_provenance(src, *conf)));
            }
        }
    }

    if want_callers {
        let prod_count = callers.iter().filter(|c| !c.is_test).count();
        let test_count = callers.len().saturating_sub(prod_count);
        let count_label = format_caller_count(callers.len(), prod_count, test_count);
        out.push_str(&format!(
            "\ncallers - functions that call {symbol} ({count_label}):\n"
        ));
        if callers.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for (i, site) in callers.iter().enumerate() {
                if i >= max {
                    out.push_str(&format!("  … (+{} more)\n", callers.len() - max));
                    break;
                }
                let tag = format_caller_tag(site);
                let line = format!("  {}   {}:{}  {tag}\n", site.name, site.rel, site.line);
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
            ..Default::default()
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

    #[test]
    fn prod_callers_sort_ahead_of_test_fixtures_and_respect_confidence() {
        assert!(is_test_path("tests/test_widget.cpp"));
        assert!(is_test_path("src/foo_test.rs"));
        assert!(is_test_path("web/app.spec.ts"));
        assert!(is_test_path("tests/fixtures/sample.cpp"));
        assert!(is_test_path("src/WidgetTests.cs"));
        assert!(!is_test_path("src/latest.rs"));
        assert!(!is_test_path("src/contest.cpp"));
        assert!(!is_test_path("src/engine/solver.cpp"));

        assert!(is_test_caller("tests::checks_widget", "src/widget.rs"));
        assert!(is_test_caller("test_foo", "src/widget.cpp"));
        assert!(!is_test_caller("run_frame", "src/engine.cpp"));

        let mut list = vec![
            CallerSite {
                name: "aaa_fixture_caller".into(),
                rel: "tests/fixtures/a.cpp".into(),
                line: 4,
                text: String::new(),
                source: EDGE_SOURCE_SCIP.into(),
                confidence: 1.0,
                is_test: is_test_caller("aaa_fixture_caller", "tests/fixtures/a.cpp"),
            },
            CallerSite {
                name: "zzz_prod_ts".into(),
                rel: "src/engine.cpp".into(),
                line: 10,
                text: String::new(),
                source: EDGE_SOURCE_TREESITTER.into(),
                confidence: CONFIDENCE_TREESITTER,
                is_test: is_test_caller("zzz_prod_ts", "src/engine.cpp"),
            },
            CallerSite {
                name: "mmm_prod_scip".into(),
                rel: "src/engine.cpp".into(),
                line: 20,
                text: String::new(),
                source: EDGE_SOURCE_SCIP.into(),
                confidence: 1.0,
                is_test: is_test_caller("mmm_prod_scip", "src/engine.cpp"),
            },
            CallerSite {
                name: "bbb_prod_namematch".into(),
                rel: "src/other.cpp".into(),
                line: 30,
                text: String::new(),
                source: EDGE_SOURCE_NAME_MATCH.into(),
                confidence: CONFIDENCE_NAME_MATCH,
                is_test: is_test_caller("bbb_prod_namematch", "src/other.cpp"),
            },
        ];
        rank_callers(&mut list);

        let ordered: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        // Production callers ALWAYS precede test/fixture callers regardless of name or confidence;
        // within production, SCIP (1.0) > treesitter (0.5) > name-match (0.3).
        assert_eq!(
            ordered,
            vec![
                "mmm_prod_scip",
                "zzz_prod_ts",
                "bbb_prod_namematch",
                "aaa_fixture_caller",
            ]
        );
    }

    #[test]
    fn k_core_and_hub_badge_classify_leaf_hub_and_core_hub() {
        // 4-clique (a, b, c, d) -> k-core = 3, plus leaf `leaf` calling `a` -> k-core = 1.
        let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut callers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let clique = ["a", "b", "c", "d"];
        for &u in &clique {
            for &v in &clique {
                if u != v {
                    callees.entry(u.into()).or_default().insert(v.into());
                    callers.entry(v.into()).or_default().insert(u.into());
                }
            }
        }
        callees.entry("leaf".into()).or_default().insert("a".into());
        callers.entry("a".into()).or_default().insert("leaf".into());

        let cores = compute_k_cores(&callees, &callers);
        assert_eq!(cores.get("a"), Some(&3));
        assert_eq!(cores.get("b"), Some(&3));
        assert_eq!(cores.get("leaf"), Some(&1));

        let leaf_hub = classify_hub(1, 1, 0, 1, 1);
        assert_eq!(leaf_hub.tier, HubTier::None);
        assert!(leaf_hub.badge().is_none());

        let clique_hub = classify_hub(4, 4, 0, 3, 3);
        assert_eq!(clique_hub.tier, HubTier::Hub);
        assert_eq!(
            clique_hub.badge().as_deref(),
            Some("[hub fan-in=4 k-core=3]")
        );

        let star_hub = classify_hub(6, 5, 1, 0, 1);
        assert_eq!(star_hub.tier, HubTier::Hub);
        assert_eq!(
            star_hub.badge().as_deref(),
            Some("[hub fan-in=6 k-core=1]")
        );

        let core_hub = classify_hub(14, 11, 3, 2, 4);
        assert_eq!(core_hub.tier, HubTier::CoreHub);
        assert_eq!(
            core_hub.badge().as_deref(),
            Some("[core-hub fan-in=14 k-core=4]")
        );
    }

    struct TempXdg {
        _lock: std::sync::MutexGuard<'static, ()>,
        xdg: PathBuf,
        prev: Option<std::ffi::OsString>,
    }

    impl TempXdg {
        fn new(tag: &str) -> Self {
            let lock = crate::cache::test_env_lock();
            let xdg = std::env::temp_dir().join(format!("wk_cg_xdg_{tag}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&xdg);
            std::fs::create_dir_all(&xdg).unwrap();
            let prev = std::env::var_os("XDG_CACHE_HOME");
            std::env::set_var("XDG_CACHE_HOME", &xdg);
            Self {
                _lock: lock,
                xdg,
                prev,
            }
        }
    }

    impl Drop for TempXdg {
        fn drop(&mut self) {
            if let Some(ref p) = self.prev {
                std::env::set_var("XDG_CACHE_HOME", p);
            }
            let _ = std::fs::remove_dir_all(&self.xdg);
        }
    }

    #[test]
    fn call_graph_build_prioritizes_prod_shows_true_totals_hub_badge_and_scip_provenance() {
        let _env = TempXdg::new("rank_scip");
        let root = std::env::temp_dir().join(format!("wk_cg_rank_scip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();

        // 5 prod callers (triggers hub badge!) + 3 test callers (alphabetically earlier names!).
        std::fs::write(
            root.join("src/core.cpp"),
            "void target() {}\n\
             void p_one() { target(); }\n\
             void p_two() { target(); }\n\
             void p_three() { target(); }\n\
             void p_four() { target(); }\n\
             void p_five() { target(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/test_core.cpp"),
            "void a_test_one() { target(); }\n\
             void a_test_two() { target(); }\n\
             void a_test_three() { target(); }\n",
        )
        .unwrap();

        // Synthetic JSON SCIP overlay upgrading p_one -> target to confidence 1.0.
        std::fs::write(
            root.join("index.scip"),
            r#"{
                "metadata": { "tool_info": { "name": "scip-clang" } },
                "edges": [
                    { "file": "src/core.cpp", "src_name": "p_one", "dst_name": "target", "line": 2, "kind": "call" }
                ]
            }"#,
        )
        .unwrap();

        let out = build(
            &root,
            &serde_json::json!({
                "symbol": "target",
                "paths": ["src", "tests"],
                "max": 4
            }),
        )
        .unwrap();

        // Hub badge + true total count (8 total: 5 prod, 3 test) even when max=4!
        assert!(out.contains("[hub fan-in=8 k-core=1]"), "{out}");
        assert!(out.contains("(8 total: 5 prod, 3 test)"), "{out}");
        // SCIP provenance displayed and ranked first among prod callers.
        assert!(out.contains("p_one   src/core.cpp:2  [scip 1.0]"), "{out}");
        assert!(out.contains("[treesitter 0.5]"), "{out}");
        // With max=4, none of the alphabetically-earlier test callers displace prod callers!
        assert!(!out.contains("a_test_one"), "{out}");
        assert!(out.contains("(+4 more)"), "{out}");

        // With higher max, test callers appear AFTER all prod callers with [test, ...] tag.
        let full = build(
            &root,
            &serde_json::json!({
                "symbol": "target",
                "paths": ["src", "tests"],
                "max": 20
            }),
        )
        .unwrap();
        let pos_prod = full.find("p_two").expect("p_two present");
        let pos_test = full.find("a_test_one").expect("a_test_one present");
        assert!(pos_prod < pos_test, "prod must precede test:\n{full}");
        assert!(full.contains("[test, treesitter 0.5]"), "{full}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rust_inline_cfg_test_mod_callers_sort_behind_prod_callers() {
        let _env = TempXdg::new("rust_inline");
        let root = std::env::temp_dir().join(format!("wk_cg_rust_inline_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/widget.rs"),
            "pub fn compute() -> i32 { 1 }\n\
             pub fn z_prod_driver() -> i32 { compute() }\n\
             #[cfg(test)]\n\
             mod tests {\n\
                 #[test]\n\
                 fn a_verifies_compute() { let _ = compute(); }\n\
             }\n",
        )
        .unwrap();

        let hop = one_hop(&root, "compute", &["src".to_string()]);
        assert_eq!(hop.caller_sites.len(), 2, "{:?}", hop.caller_sites);
        assert_eq!(hop.caller_sites[0].name, "z_prod_driver");
        assert!(!hop.caller_sites[0].is_test);
        assert_eq!(hop.caller_sites[1].name, "tests::a_verifies_compute");
        assert!(hop.caller_sites[1].is_test);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scip_disambiguates_conflicting_qualifiers_and_treesitter_downgrades_to_name_match() {
        // Conflicting qualifiers on SCIP edge -> rejected (no false positive).
        assert!(match_edge_name("Physics::tick", "Audio::tick", EDGE_SOURCE_SCIP, 1.0).is_none());
        // Matching suffix on SCIP edge -> kept at confidence 1.0.
        assert_eq!(
            match_edge_name("engine::Audio::tick", "Audio::tick", EDGE_SOURCE_SCIP, 1.0),
            Some((EDGE_SOURCE_SCIP.to_string(), 1.0))
        );
        // Conflicting qualifiers on tree-sitter edge -> downgraded to name-match (0.3).
        assert_eq!(
            match_edge_name(
                "Physics::tick",
                "Audio::tick",
                EDGE_SOURCE_TREESITTER,
                CONFIDENCE_TREESITTER
            ),
            Some((EDGE_SOURCE_NAME_MATCH.to_string(), CONFIDENCE_NAME_MATCH))
        );
    }
}
