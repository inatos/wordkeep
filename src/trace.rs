//! trace_summary - condense a Tracy CSV export into the hottest zones.
//!
//! Tracy's `csvexport` emits one row per zone with columns:
//!   name,src_file,src_line,total_ns,total_perc,counts,mean_ns,min_ns,max_ns,std_ns
//! This tool parses that, sorts by a chosen key, and returns a compact,
//! token-budgeted table so an agent can see "what dominates the frame" without
//! ingesting the whole CSV. With `baseline` set, it diffs two captures instead,
//! surfacing per-zone regressions.
//!
//! Either input may be a raw `.tracy` capture rather than a pre-exported CSV; in
//! that case we shell out to Tracy's own `tracy-csvexport` (override the binary
//! via `WORDKEEP_TRACY_CSVEXPORT`) and parse its stdout, so the native
//! format never has to be re-decoded here.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::stats;

struct Row {
    name: String,
    src_file: String,
    src_line: String,
    total_ns: f64,
    counts: u64,
    mean_ns: f64,
    max_ns: f64,
}

pub fn summary(root: &Path, args: &Value) -> Result<String, String> {
    let top = args.get("top").and_then(Value::as_u64).unwrap_or(15).max(1) as usize;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;
    let sort_by = args
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("total")
        .to_lowercase();
    let dir = args.get("dir").and_then(Value::as_str).unwrap_or("debug");
    let file = resolve_file(root, dir, args.get("file").and_then(Value::as_str))?;

    // Two-capture diff mode: compare the current file against a baseline.
    if let Some(b) = args.get("baseline").and_then(Value::as_str) {
        let base = resolve_file(root, dir, Some(b))?;
        return diff(root, &file, &base, top, budget);
    }

    let text = read_trace(&file)?;
    let mut rows = parse(&text)?;
    if rows.is_empty() {
        return Ok(format!("trace_summary: no zones in {}", file.display()));
    }

    let grand: f64 = rows.iter().map(|r| r.total_ns).sum();
    match sort_by.as_str() {
        "mean" => rows.sort_by_key(|r| std::cmp::Reverse(r.mean_ns.to_bits())),
        "max" => rows.sort_by_key(|r| std::cmp::Reverse(r.max_ns.to_bits())),
        "count" => rows.sort_by_key(|r| std::cmp::Reverse(r.counts)),
        _ => rows.sort_by_key(|r| std::cmp::Reverse(r.total_ns.to_bits())),
    }

    let rel = file
        .strip_prefix(root)
        .unwrap_or(&file)
        .to_string_lossy()
        .replace('\\', "/");
    let shown = top.min(rows.len());
    let mut out = format!(
        "trace_summary - {rel}\n{} zones, sorted by {sort_by}; share = % of summed zone time\ntop {shown}:\n\n",
        rows.len()
    );
    out.push_str(&format!(
        "{:<30} {:>10} {:>7} {:>8} {:>10}  {}\n",
        "zone", "total ms", "share", "calls", "mean us", "site"
    ));
    let mut used = out.len() / 4;
    for (i, r) in rows.iter().take(top).enumerate() {
        let share = if grand > 0.0 {
            r.total_ns / grand * 100.0
        } else {
            0.0
        };
        let line = format!(
            "{:<30} {:>10.3} {:>6.2}% {:>8} {:>10.1}  {}:{}\n",
            truncate(&r.name, 30),
            r.total_ns / 1e6,
            share,
            r.counts,
            r.mean_ns / 1e3,
            basename(&r.src_file),
            r.src_line
        );
        let lt = line.len() / 4;
        if used + lt > budget && i > 0 {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
        used += lt;
        out.push_str(&line);
    }
    if sort_by == "total" {
        out.push_str(
            "\nTip: user reports hitches → re-run with sort_by: \"max\" (mean can hide spikes). \
             Or use trace_profile for max sort + diff_map + index_stale in one call.\n",
        );
    }
    stats::record(
        "trace_summary",
        (text.len() / 4) as u64,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

/// Diff two captures by zone name, sorted so the biggest total-time regressions
/// come first. New/removed zones are flagged.
fn diff(
    root: &Path,
    new_file: &Path,
    base_file: &Path,
    top: usize,
    budget: usize,
) -> Result<String, String> {
    let new_text = read_trace(new_file)?;
    let base_text = read_trace(base_file)?;
    let new_rows = parse(&new_text)?;
    let base_rows = parse(&base_text)?;
    let a_new = aggregate(&new_rows);
    let a_base = aggregate(&base_rows);

    let mut names: HashSet<&str> = HashSet::new();
    names.extend(a_new.keys().map(String::as_str));
    names.extend(a_base.keys().map(String::as_str));

    let mut items: Vec<DiffRow> = Vec::with_capacity(names.len());
    let mut net = 0.0f64;
    for name in names {
        let n = a_new.get(name);
        let b = a_base.get(name);
        let nt = n.map(|a| a.total_ns).unwrap_or(0.0);
        let bt = b.map(|a| a.total_ns).unwrap_or(0.0);
        let delta = nt - bt;
        net += delta;
        let pct = if bt > 0.0 {
            Some(delta / bt * 100.0)
        } else {
            None
        };
        let nmean = n.map(|a| a.mean_us()).unwrap_or(0.0);
        let bmean = b.map(|a| a.mean_us()).unwrap_or(0.0);
        let flag = match (n.is_some(), b.is_some()) {
            (true, false) => "new",
            (false, true) => "gone",
            _ => "",
        };
        items.push(DiffRow {
            name: name.to_string(),
            base_ms: bt / 1e6,
            new_ms: nt / 1e6,
            delta_ms: delta / 1e6,
            pct,
            dmean_us: nmean - bmean,
            flag,
        });
    }
    // Regressions (largest positive delta) first.
    items.sort_by(|a, b| b.delta_ms.total_cmp(&a.delta_ms));

    let new_rel = rel_to(root, new_file);
    let base_rel = rel_to(root, base_file);
    let shown = top.min(items.len());
    let mut out = format!(
        "trace_summary diff - new {new_rel} vs base {base_rel}\n{} zones (union); sorted by Δ total ms (regressions first); net Δ {:+.3} ms\ntop {shown}:\n\n",
        items.len(),
        net / 1e6
    );
    out.push_str(&format!(
        "{:<28} {:>10} {:>10} {:>10} {:>8} {:>11}  {}\n",
        "zone", "base ms", "new ms", "Δ ms", "Δ%", "Δ mean us", "flag"
    ));
    let mut used = out.len() / 4;
    for (i, d) in items.iter().take(top).enumerate() {
        let pct = match d.pct {
            Some(p) => format!("{p:>+7.1}%"),
            None => format!("{:>8}", "-"),
        };
        let line = format!(
            "{:<28} {:>10.3} {:>10.3} {:>+10.3} {pct} {:>+11.1}  {}\n",
            truncate(&d.name, 28),
            d.base_ms,
            d.new_ms,
            d.delta_ms,
            d.dmean_us,
            d.flag
        );
        let lt = line.len() / 4;
        if used + lt > budget && i > 0 {
            out.push_str("… (truncated by token_budget)\n");
            break;
        }
        used += lt;
        out.push_str(&line);
    }
    stats::record(
        "trace_summary",
        ((new_text.len() + base_text.len()) / 4) as u64,
        (out.len() / 4) as u64,
    );
    Ok(out)
}

struct Agg {
    total_ns: f64,
    counts: u64,
}

impl Agg {
    fn mean_us(&self) -> f64 {
        if self.counts > 0 {
            self.total_ns / self.counts as f64 / 1e3
        } else {
            0.0
        }
    }
}

struct DiffRow {
    name: String,
    base_ms: f64,
    new_ms: f64,
    delta_ms: f64,
    pct: Option<f64>,
    dmean_us: f64,
    flag: &'static str,
}

/// Sum total_ns and counts per zone name (Tracy usually emits one row per zone,
/// but tolerate duplicates).
fn aggregate(rows: &[Row]) -> HashMap<String, Agg> {
    let mut m: HashMap<String, Agg> = HashMap::new();
    for r in rows {
        let e = m.entry(r.name.clone()).or_insert(Agg {
            total_ns: 0.0,
            counts: 0,
        });
        e.total_ns += r.total_ns;
        e.counts += r.counts;
    }
    m
}

/// Read a capture as CSV text. A `.tracy` file is exported on the fly via
/// `tracy-csvexport`; anything else is read straight from disk.
fn read_trace(p: &Path) -> Result<String, String> {
    if p.extension().and_then(|e| e.to_str()) == Some("tracy") {
        let exporter = std::env::var("WORDKEEP_TRACY_CSVEXPORT")
            .unwrap_or_else(|_| "tracy-csvexport".to_string());
        let out = std::process::Command::new(&exporter)
            .arg(p)
            .output()
            .map_err(|e| {
                format!(
                    "run {exporter} on {}: {e} (is tracy-csvexport on PATH?)",
                    p.display()
                )
            })?;
        if !out.status.success() {
            return Err(format!(
                "{exporter} failed on {}: {}",
                p.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return String::from_utf8(out.stdout)
            .map_err(|e| format!("{exporter} produced non-UTF-8 output: {e}"));
    }
    std::fs::read_to_string(p).map_err(|e| format!("read {}: {e}", p.display()))
}

fn rel_to(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn parse(text: &str) -> Result<Vec<Row>, String> {
    let mut lines = text.lines();
    let header = lines.next().ok_or("empty file")?;
    let cols: Vec<&str> = header.split(',').collect();
    let idx = |name: &str| cols.iter().position(|c| c.trim() == name);
    let i_name = idx("name").ok_or("missing 'name' column")?;
    let i_total = idx("total_ns").ok_or("missing 'total_ns' column")?;
    let i_file = idx("src_file");
    let i_line = idx("src_line");
    let i_counts = idx("counts");
    let i_mean = idx("mean_ns");
    let i_max = idx("max_ns");

    let mut rows = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < cols.len() {
            continue;
        }
        let get = |i: Option<usize>| i.and_then(|i| f.get(i)).copied().unwrap_or("").trim();
        rows.push(Row {
            name: f.get(i_name).copied().unwrap_or("").trim().to_string(),
            src_file: get(i_file).to_string(),
            src_line: get(i_line).to_string(),
            total_ns: f
                .get(i_total)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0.0),
            counts: get(i_counts).parse().unwrap_or(0),
            mean_ns: get(i_mean).parse().unwrap_or(0.0),
            max_ns: get(i_max).parse().unwrap_or(0.0),
        });
    }
    Ok(rows)
}

fn resolve_file(root: &Path, dir: &str, file: Option<&str>) -> Result<PathBuf, String> {
    if let Some(f) = file {
        let p = Path::new(f);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            let direct = root.join(f);
            if direct.exists() {
                direct
            } else {
                root.join(dir).join(f)
            }
        };
        return if full.exists() {
            Ok(full)
        } else {
            Err(format!("trace file not found: {f}"))
        };
    }
    // newest *.csv in dir
    let d = root.join(dir);
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = std::fs::read_dir(&d).map_err(|e| format!("read_dir {}: {e}", d.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("csv") {
            continue;
        }
        let m = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |(bm, _)| m > *bm) {
            best = Some((m, p));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| format!("no .csv files in {}", d.display()))
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HEADER: &str =
        "name,src_file,src_line,total_ns,total_perc,counts,mean_ns,min_ns,max_ns,std_ns";

    fn csv(rows: &[&str]) -> String {
        let mut s = String::from(HEADER);
        s.push('\n');
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    #[test]
    fn parse_reads_named_columns() {
        let text = csv(&["PhysicsStep,/a/phys.cpp,12,2000,50.0,100,20,1,40,3"]);
        let rows = parse(&text).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.name, "PhysicsStep");
        assert_eq!(r.src_file, "/a/phys.cpp");
        assert_eq!(r.src_line, "12");
        assert_eq!(r.total_ns, 2000.0);
        assert_eq!(r.counts, 100);
        assert_eq!(r.mean_ns, 20.0);
        assert_eq!(r.max_ns, 40.0);
    }

    #[test]
    fn parse_skips_short_and_blank_lines() {
        let text = format!("{HEADER}\n\nBad,row\nGood,/x.cpp,1,10,1.0,1,10,1,10,0\n");
        let rows = parse(&text).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Good");
    }

    #[test]
    fn aggregate_sums_duplicates() {
        let rows = parse(&csv(&[
            "Z,/x.cpp,1,100,1.0,2,50,1,60,0",
            "Z,/x.cpp,1,300,1.0,4,75,1,90,0",
        ]))
        .unwrap();
        let agg = aggregate(&rows);
        let z = &agg["Z"];
        assert_eq!(z.total_ns, 400.0);
        assert_eq!(z.counts, 6);
    }

    #[test]
    fn summary_sorts_by_total() {
        let dir = tmp_dir("trace_summary_total");
        write(
            &dir.join("t.csv"),
            &csv(&[
                "Small,/x.cpp,1,1000,10,10,100,1,200,0",
                "Big,/y.cpp,2,9000,90,10,900,1,1800,0",
            ]),
        );
        let out = summary(&dir, &json!({"file": "t.csv", "dir": "."})).unwrap();
        let big = out.find("Big").unwrap();
        let small = out.find("Small").unwrap();
        assert!(big < small, "Big should sort before Small:\n{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_diff_flags_new_and_regression() {
        let dir = tmp_dir("trace_summary_diff");
        write(
            &dir.join("base.csv"),
            &csv(&["A,/x.cpp,1,1000,100,10,100,1,200,0"]),
        );
        write(
            &dir.join("cur.csv"),
            &csv(&[
                "A,/x.cpp,1,3000,75,10,300,1,600,0",
                "B,/y.cpp,2,1000,25,10,100,1,200,0",
            ]),
        );
        let out = summary(
            &dir,
            &json!({"file": "cur.csv", "baseline": "base.csv", "dir": "."}),
        )
        .unwrap();
        assert!(out.contains("diff"), "{out}");
        assert!(out.contains("new"), "B should be flagged new:\n{out}");
        // A regressed by +2ms, B is +1ms new → A first.
        let a = out.find("\nA ").or_else(|| out.find("A   ")).unwrap();
        let b = out.find('B').unwrap();
        assert!(a < b, "regression A should sort first:\n{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_picks_newest_csv() {
        let dir = tmp_dir("trace_resolve");
        write(&dir.join("old.csv"), &csv(&["A,/x.cpp,1,1,1,1,1,1,1,0"]));
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&dir.join("new.csv"), &csv(&["A,/x.cpp,1,1,1,1,1,1,1,0"]));
        let p = resolve_file(&dir, ".", None).unwrap();
        assert_eq!(p.file_name().unwrap(), "new.csv");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn basename_and_truncate() {
        assert_eq!(basename("/a/b/c.cpp"), "c.cpp");
        assert_eq!(basename("c.cpp"), "c.cpp");
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cbtest_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }
}
