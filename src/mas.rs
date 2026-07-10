//! Recursive MAS blackboard - session-scoped, token-capped state handoff between
//! subagents (Planner/Critic/Solver) without re-pasting bulky text.
//!
//! Sessions live under `$XDG_CACHE_HOME/wordkeep/mas/<session>.json`. Each call
//! reloads from disk so subagents that spawn separate MCP processes still share
//! state; writes are atomic (temp + rename).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{cache, knowledge, stats};

const STORE_VERSION: u64 = 1;
const DEFAULT_MAX_ROUNDS: u64 = 3;
const DEFAULT_ENTRY_TOKENS: usize = 400;
const DEFAULT_READ_BUDGET: usize = 1200;

static SESSION_CACHE: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct Entry {
    id: u64,
    round: u64,
    role: String,
    ts: u64,
    summary: String,
    claims: Vec<String>,
    decisions: Vec<String>,
    open_questions: Vec<String>,
    anchors: Vec<String>,
    handoff_to: Option<String>,
    tags: Vec<String>,
    approved: Option<bool>,
}

#[derive(Clone, Debug)]
struct Session {
    session: String,
    created: u64,
    updated: u64,
    round: u64,
    max_rounds: u64,
    status: String,
    entries: Vec<Entry>,
    result: Option<String>,
    next_id: u64,
}

fn cache_cell() -> &'static Mutex<HashMap<String, Session>> {
    SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn entry_token_cap() -> usize {
    std::env::var("WORDKEEP_MAS_ENTRY_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_ENTRY_TOKENS)
}

fn mas_dir() -> PathBuf {
    cache::dir().join("mas")
}

fn session_path(session: &str) -> PathBuf {
    mas_dir().join(format!("{session}.json"))
}

/// Validate session id: slug `[A-Za-z0-9._-]`, no `/`, no `..`.
pub fn validate_session_id(session: &str) -> Result<(), String> {
    let s = session.trim();
    if s.is_empty() {
        return Err("session is required".into());
    }
    if s.contains('/') || s.contains('\\') || s.contains("..") {
        return Err("session must be a slug (no /, \\, or ..)".into());
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err("session must match [A-Za-z0-9._-]".into());
    }
    Ok(())
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens.saturating_mul(4);
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(16)).collect();
    out.push_str(" … (truncated)");
    out
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    optional_str(args, key).ok_or_else(|| format!("{key} is required"))
}

fn write_atomic(path: &Path, doc: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(doc).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

fn entry_to_value(e: &Entry) -> Value {
    json!({
        "id": e.id,
        "round": e.round,
        "role": e.role,
        "ts": e.ts,
        "summary": e.summary,
        "claims": e.claims,
        "decisions": e.decisions,
        "open_questions": e.open_questions,
        "anchors": e.anchors,
        "handoff_to": e.handoff_to,
        "tags": e.tags,
        "approved": e.approved,
    })
}

fn entry_from_value(v: &Value) -> Option<Entry> {
    Some(Entry {
        id: v.get("id").and_then(Value::as_u64)?,
        round: v.get("round").and_then(Value::as_u64).unwrap_or(1),
        role: v.get("role").and_then(Value::as_str)?.to_string(),
        ts: v.get("ts").and_then(Value::as_u64).unwrap_or(0),
        summary: v
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        claims: string_array(v, "claims"),
        decisions: string_array(v, "decisions"),
        open_questions: string_array(v, "open_questions"),
        anchors: string_array(v, "anchors"),
        handoff_to: optional_str(v, "handoff_to"),
        tags: string_array(v, "tags"),
        approved: v.get("approved").and_then(Value::as_bool),
    })
}

fn session_to_value(s: &Session) -> Value {
    json!({
        "version": STORE_VERSION,
        "session": s.session,
        "created": s.created,
        "updated": s.updated,
        "round": s.round,
        "max_rounds": s.max_rounds,
        "status": s.status,
        "entries": s.entries.iter().map(entry_to_value).collect::<Vec<_>>(),
        "result": s.result,
        "next_id": s.next_id,
    })
}

fn session_from_value(v: &Value) -> Option<Session> {
    let entries: Vec<Entry> = v
        .get("entries")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(entry_from_value).collect())
        .unwrap_or_default();
    let next_id = v
        .get("next_id")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| entries.iter().map(|e| e.id).max().unwrap_or(0) + 1);
    Some(Session {
        session: v.get("session").and_then(Value::as_str)?.to_string(),
        created: v.get("created").and_then(Value::as_u64).unwrap_or(0),
        updated: v.get("updated").and_then(Value::as_u64).unwrap_or(0),
        round: v.get("round").and_then(Value::as_u64).unwrap_or(1),
        max_rounds: v
            .get("max_rounds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_ROUNDS),
        status: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("active")
            .to_string(),
        entries,
        result: v.get("result").and_then(Value::as_str).map(String::from),
        next_id,
    })
}

fn load_session(session: &str) -> Result<Option<Session>, String> {
    validate_session_id(session)?;
    if let Ok(cache) = cache_cell().lock() {
        if let Some(s) = cache.get(session) {
            return Ok(Some(s.clone()));
        }
    }
    let path = session_path(session);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    session_from_value(&v)
        .ok_or_else(|| format!("invalid session file {}", path.display()))
        .map(Some)
}

fn save_session(s: &Session) -> Result<(), String> {
    let path = session_path(&s.session);
    write_atomic(&path, &session_to_value(s))?;
    if let Ok(mut cache) = cache_cell().lock() {
        cache.insert(s.session.clone(), s.clone());
    }
    Ok(())
}

fn get_or_create_session(session: &str, max_rounds: Option<u64>) -> Result<Session, String> {
    validate_session_id(session)?;
    if let Ok(cache) = cache_cell().lock() {
        if let Some(s) = cache.get(session) {
            return Ok(s.clone());
        }
    }
    if let Some(mut s) = load_session(session)? {
        if let Some(mr) = max_rounds {
            s.max_rounds = mr.max(1);
        }
        return Ok(s);
    }
    let now = now_secs();
    let mr = max_rounds.unwrap_or(DEFAULT_MAX_ROUNDS).max(1);
    Ok(Session {
        session: session.to_string(),
        created: now,
        updated: now,
        round: 1,
        max_rounds: mr,
        status: "active".into(),
        entries: Vec::new(),
        result: None,
        next_id: 1,
    })
}

fn entry_text(e: &Entry) -> String {
    let mut out = format!(
        "[{}] round {} role={} id={}\n{}\n",
        e.ts, e.round, e.role, e.id, e.summary
    );
    if !e.claims.is_empty() {
        out.push_str("claims:\n");
        for c in &e.claims {
            out.push_str(&format!("  - {c}\n"));
        }
    }
    if !e.decisions.is_empty() {
        out.push_str("decisions:\n");
        for d in &e.decisions {
            out.push_str(&format!("  - {d}\n"));
        }
    }
    if !e.open_questions.is_empty() {
        out.push_str("open_questions:\n");
        for q in &e.open_questions {
            out.push_str(&format!("  - {q}\n"));
        }
    }
    if !e.anchors.is_empty() {
        out.push_str(&format!("anchors: {}\n", e.anchors.join(", ")));
    }
    if let Some(h) = &e.handoff_to {
        out.push_str(&format!("handoff_to: {h}\n"));
    }
    if !e.tags.is_empty() {
        out.push_str(&format!("tags: {}\n", e.tags.join(", ")));
    }
    if let Some(a) = e.approved {
        out.push_str(&format!("approved: {a}\n"));
    }
    out
}

fn role_counts(entries: &[Entry]) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for e in entries {
        *m.entry(e.role.clone()).or_insert(0) += 1;
    }
    m
}

fn convergence_hint(s: &Session) -> String {
    let latest_critic = s
        .entries
        .iter()
        .rev()
        .find(|e| e.role.eq_ignore_ascii_case("critic"));
    match latest_critic {
        Some(e) if e.approved == Some(true) && e.open_questions.is_empty() => {
            "likely converged (critic approved, no open questions)".into()
        }
        Some(e) if e.approved == Some(true) => {
            format!(
                "partial convergence (critic approved, {} open question(s))",
                e.open_questions.len()
            )
        }
        Some(_) => "not converged (critic has not approved)".into(),
        None => "not converged (no critic entry yet)".into(),
    }
}

fn cap_entry_fields(
    summary: &mut String,
    claims: &mut Vec<String>,
    decisions: &mut Vec<String>,
    open_questions: &mut Vec<String>,
    max_tokens: usize,
) {
    *summary = truncate_to_tokens(summary, max_tokens);
    let mut used = estimate_tokens(summary);

    for list in [&mut *claims, &mut *decisions, &mut *open_questions] {
        let mut kept = Vec::new();
        for item in list.drain(..) {
            let item_tokens = estimate_tokens(&item);
            if used + item_tokens > max_tokens {
                break;
            }
            used += item_tokens;
            kept.push(item);
        }
        *list = kept;
    }
}

/// Append a compact entry to a session blackboard.
pub fn post(_root: &Path, args: &Value) -> Result<String, String> {
    let session = required_str(args, "session")?;
    let role = required_str(args, "role")?;
    let mut summary = required_str(args, "summary")?;
    let mut claims = string_array(args, "claims");
    let mut decisions = string_array(args, "decisions");
    let mut open_questions = string_array(args, "open_questions");
    let anchors = string_array(args, "anchors");
    let handoff_to = optional_str(args, "handoff_to");
    let tags = string_array(args, "tags");
    let approved = args.get("approved").and_then(Value::as_bool);

    let max_rounds = args
        .get("max_rounds")
        .and_then(Value::as_u64)
        .map(|r| r.max(1));
    let mut s = get_or_create_session(&session, max_rounds)?;

    if s.status == "final" {
        return Err(format!(
            "session {session} is finalized; open a new session"
        ));
    }
    if s.round > s.max_rounds {
        return Err(format!(
            "session {session} is past max_rounds ({}/{})",
            s.round, s.max_rounds
        ));
    }

    let cap = entry_token_cap();
    cap_entry_fields(
        &mut summary,
        &mut claims,
        &mut decisions,
        &mut open_questions,
        cap,
    );

    let id = s.next_id;
    s.next_id += 1;
    let now = now_secs();
    let entry = Entry {
        id,
        round: s.round,
        role: role.clone(),
        ts: now,
        summary: summary.clone(),
        claims: claims.clone(),
        decisions: decisions.clone(),
        open_questions: open_questions.clone(),
        anchors,
        handoff_to: handoff_to.clone(),
        tags,
        approved,
    };
    let distilled = estimate_tokens(&summary)
        + claims.iter().map(|c| estimate_tokens(c)).sum::<usize>()
        + decisions.iter().map(|d| estimate_tokens(d)).sum::<usize>()
        + open_questions
            .iter()
            .map(|q| estimate_tokens(q))
            .sum::<usize>();

    s.entries.push(entry);
    s.updated = now;
    save_session(&s)?;

    let out = format!(
        "mas_post - session {session}, entry id={id}, round {}/{}, role {role:?}{}",
        s.round,
        s.max_rounds,
        handoff_to
            .map(|h| format!(", handoff_to {h:?}"))
            .unwrap_or_default()
    );
    stats::record("mas_post", distilled as u64, estimate_tokens(&out) as u64);
    Ok(out)
}

/// Read session blackboard entries, token-budgeted, newest-first.
pub fn read(_root: &Path, args: &Value) -> Result<String, String> {
    let session = required_str(args, "session")?;
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_BUDGET as u64) as usize;
    let recipient = optional_str(args, "recipient");
    let role = optional_str(args, "role");
    let round = args.get("round").and_then(Value::as_u64);
    let tag = optional_str(args, "tag");
    let since_id = args.get("since_id").and_then(Value::as_u64);

    let s = load_session(&session)?.ok_or_else(|| format!("session {session} not found"))?;

    let mut filtered: Vec<&Entry> = s.entries.iter().collect();
    if let Some(r) = &recipient {
        filtered.retain(|e| {
            e.handoff_to
                .as_ref()
                .is_some_and(|h| h.eq_ignore_ascii_case(r))
        });
    }
    if let Some(r) = &role {
        filtered.retain(|e| e.role.eq_ignore_ascii_case(r));
    }
    if let Some(r) = round {
        filtered.retain(|e| e.round == r);
    }
    if let Some(t) = &tag {
        filtered.retain(|e| e.tags.iter().any(|x| x.eq_ignore_ascii_case(t)));
    }
    if let Some(sid) = since_id {
        filtered.retain(|e| e.id > sid);
    }
    filtered.sort_by(|a, b| b.id.cmp(&a.id));

    let distilled: u64 = s
        .entries
        .iter()
        .map(|e| estimate_tokens(&entry_text(e)) as u64)
        .sum();
    let mut out = format!(
        "mas_read - session {session}, round {}/{}, status {}, {} match(es)\n",
        s.round,
        s.max_rounds,
        s.status,
        filtered.len()
    );
    let mut used = estimate_tokens(&out);
    let mut shown = 0usize;
    for e in filtered {
        let block = format!("\n---\n{}", entry_text(e));
        let block_tokens = estimate_tokens(&block);
        if used + block_tokens > budget && shown > 0 {
            out.push_str("\n… (further entries omitted by token_budget)\n");
            break;
        }
        used += block_tokens;
        shown += 1;
        out.push_str(&block);
    }
    if shown == 0 {
        out.push_str("\n(no matching entries)\n");
    }
    stats::record("mas_read", distilled, estimate_tokens(&out) as u64);
    Ok(out)
}

/// Recursion bookkeeping: round, per-role counts, convergence hint.
pub fn status(_root: &Path, args: &Value) -> Result<String, String> {
    let session = required_str(args, "session")?;
    let max_rounds = args
        .get("max_rounds")
        .and_then(Value::as_u64)
        .map(|r| r.max(1));
    let advance = args
        .get("advance_round")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut s = get_or_create_session(&session, max_rounds)?;
    let existed = load_session(&session)?.is_some();

    if advance {
        if s.status == "final" {
            return Err(format!("session {session} is finalized"));
        }
        if s.round >= s.max_rounds {
            return Err(format!(
                "session {session} at max_rounds ({}/{})",
                s.round, s.max_rounds
            ));
        }
        s.round += 1;
        s.updated = now_secs();
        save_session(&s)?;
    } else if !existed {
        save_session(&s)?;
    }

    let counts = role_counts(&s.entries);
    let mut count_lines = String::new();
    let mut roles: Vec<_> = counts.keys().collect();
    roles.sort();
    for r in roles {
        count_lines.push_str(&format!("  {r}: {}\n", counts[r]));
    }
    if count_lines.is_empty() {
        count_lines.push_str("  (none)\n");
    }

    let rounds_left = s.max_rounds.saturating_sub(s.round);
    let hint = convergence_hint(&s);
    let out = format!(
        "mas_status - session {session}\n\
         status: {}\n\
         round: {}/{}\n\
         rounds_remaining: {rounds_left}\n\
         entry_cap_tokens: {}\n\
         entries_by_role:\n{count_lines}\
         convergence: {hint}\n",
        s.status,
        s.round,
        s.max_rounds,
        entry_token_cap(),
    );
    stats::record(
        "mas_status",
        s.entries.len() as u64 * 64,
        estimate_tokens(&out) as u64,
    );
    Ok(out)
}

/// Finalize a session and optionally promote the result to `.wordkeep/notes/`.
pub fn finalize(root: &Path, args: &Value) -> Result<String, String> {
    let session = required_str(args, "session")?;
    let result = required_str(args, "result")?;
    let promote = args
        .get("promote")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let note_path = optional_str(args, "note_path");
    let note_heading = optional_str(args, "note_heading");

    let mut s = load_session(&session)?.ok_or_else(|| format!("session {session} not found"))?;
    if s.status == "final" {
        return Err(format!("session {session} is already finalized"));
    }

    s.status = "final".into();
    s.result = Some(result.clone());
    s.updated = now_secs();
    save_session(&s)?;

    let mut out = format!(
        "mas_finalize - session {session} marked final ({} entries, round {}/{})\n",
        s.entries.len(),
        s.round,
        s.max_rounds
    );

    if promote {
        let rel = note_path.unwrap_or_else(|| format!(".wordkeep/notes/mas-{session}.md"));
        let heading = note_heading.unwrap_or_else(|| format!("MAS session {session}"));
        let body = format!(
            "**Result:**\n\n{result}\n\n**Rounds:** {}/{}\n\n**Entries:** {}\n",
            s.round,
            s.max_rounds,
            s.entries.len()
        );
        let upsert_args = json!({
            "path": rel,
            "heading": heading,
            "body": body,
            "mode": "upsert_section",
        });
        match knowledge::upsert(root, &upsert_args) {
            Ok(msg) => out.push_str(&format!("promoted: {msg}\n")),
            Err(e) => out.push_str(&format!("promote failed: {e}\n")),
        }
    }

    let distilled = estimate_tokens(&result) + s.entries.len() * 64;
    stats::record(
        "mas_finalize",
        distilled as u64,
        estimate_tokens(&out) as u64,
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);
    static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn isolated_cache() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("cbtest_mas_{}_{}", std::process::id(), id))
    }

    fn with_cache<F: FnOnce()>(f: F) {
        let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cache_home = isolated_cache();
        std::env::set_var("XDG_CACHE_HOME", &cache_home);
        if let Ok(mut c) = cache_cell().lock() {
            c.clear();
        }
        f();
        let _ = std::fs::remove_dir_all(&cache_home);
    }

    #[test]
    fn validate_session_id_rejects_bad_slugs() {
        assert!(validate_session_id("good-slug_1.2").is_ok());
        assert!(validate_session_id("../evil").is_err());
        assert!(validate_session_id("has/slash").is_err());
        assert!(validate_session_id("bad space").is_err());
        assert!(validate_session_id("").is_err());
    }

    #[test]
    fn post_respects_entry_token_cap() {
        with_cache(|| {
            let long = "word ".repeat(500);
            let args = json!({
                "session": "cap-test",
                "role": "planner",
                "summary": long,
            });
            post(Path::new("."), &args).unwrap();
            let s = load_session("cap-test").unwrap().unwrap();
            assert!(s.entries[0].summary.contains("(truncated)"));
        });
    }

    #[test]
    fn round_advance_and_budget() {
        with_cache(|| {
            status(
                Path::new("."),
                &json!({"session": "round-test", "max_rounds": 2}),
            )
            .unwrap();
            post(
                Path::new("."),
                &json!({"session": "round-test", "role": "planner", "summary": "plan"}),
            )
            .unwrap();
            status(
                Path::new("."),
                &json!({"session": "round-test", "advance_round": true}),
            )
            .unwrap();
            let s = load_session("round-test").unwrap().unwrap();
            assert_eq!(s.round, 2);
            status(
                Path::new("."),
                &json!({"session": "round-test", "advance_round": true}),
            )
            .unwrap_err();
        });
    }

    #[test]
    fn read_filters_by_recipient_role_round_tag_since_id() {
        with_cache(|| {
            post(
                Path::new("."),
                &json!({
                    "session": "filter-test",
                    "role": "planner",
                    "summary": "for solver",
                    "handoff_to": "solver",
                    "tags": ["alpha"],
                }),
            )
            .unwrap();
            post(
                Path::new("."),
                &json!({
                    "session": "filter-test",
                    "role": "solver",
                    "summary": "for critic",
                    "handoff_to": "critic",
                    "tags": ["beta"],
                }),
            )
            .unwrap();

            let by_recipient = read(
                Path::new("."),
                &json!({"session": "filter-test", "recipient": "solver"}),
            )
            .unwrap();
            assert!(by_recipient.contains("for solver"));
            assert!(!by_recipient.contains("for critic"));

            let by_role = read(
                Path::new("."),
                &json!({"session": "filter-test", "role": "solver"}),
            )
            .unwrap();
            assert!(by_role.contains("for critic"));

            let by_tag = read(
                Path::new("."),
                &json!({"session": "filter-test", "tag": "alpha"}),
            )
            .unwrap();
            assert!(by_tag.contains("for solver"));

            let since = read(
                Path::new("."),
                &json!({"session": "filter-test", "since_id": 1}),
            )
            .unwrap();
            assert!(!since.contains("for solver"));
            assert!(since.contains("for critic"));
        });
    }

    #[test]
    fn read_uses_less_than_full_repost_baseline() {
        with_cache(|| {
            for (role, summary, handoff) in [
                ("planner", "Plan: refactor widget_area", "solver"),
                ("solver", "Solution: extract helper fn", "critic"),
                ("critic", "Approved with one nit", "planner"),
            ] {
                post(
                    Path::new("."),
                    &json!({
                        "session": "bench-test",
                        "role": role,
                        "summary": summary,
                        "handoff_to": handoff,
                    }),
                )
                .unwrap();
            }
            let filtered = read(
                Path::new("."),
                &json!({"session": "bench-test", "recipient": "solver", "token_budget": 1200}),
            )
            .unwrap();
            let full = read(
                Path::new("."),
                &json!({"session": "bench-test", "token_budget": 1200}),
            )
            .unwrap();
            let filtered_tokens = filtered.len() / 4;
            let full_tokens = full.len() / 4;
            assert!(
                filtered_tokens < full_tokens,
                "recipient-filtered read ({filtered_tokens} tok) should beat full blackboard ({full_tokens} tok)"
            );
            assert_eq!(filtered.matches("Plan: refactor").count(), 1);
            assert!(!filtered.contains("Approved with one nit"));
        });
    }

    #[test]
    fn finalize_marks_session_final() {
        with_cache(|| {
            post(
                Path::new("."),
                &json!({"session": "fin-test", "role": "planner", "summary": "done"}),
            )
            .unwrap();
            let out = finalize(
                Path::new("."),
                &json!({"session": "fin-test", "result": "ship it"}),
            )
            .unwrap();
            assert!(out.contains("marked final"));
            let s = load_session("fin-test").unwrap().unwrap();
            assert_eq!(s.status, "final");
            assert_eq!(s.result.as_deref(), Some("ship it"));
            post(
                Path::new("."),
                &json!({"session": "fin-test", "role": "planner", "summary": "nope"}),
            )
            .unwrap_err();
        });
    }
}
