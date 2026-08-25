//! Privileged, opt-in attach helper for Runtime Memory Health.
//!
//! The wiki never elevates. Run this helper explicitly (or through sudo when
//! required by Yama/eBPF policy) and pass the per-serve runtime token.

use clap::{Parser, ValueEnum};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Backend {
    Auto,
    Bpftrace,
    Perf,
    Etw,
}

#[derive(Debug, Parser)]
#[command(
    name = "wordkeep-runtime-helper",
    about = "Opt-in allocation stream helper for Wordkeep Runtime Health"
)]
struct Cli {
    /// Target process id.
    #[arg(long)]
    pid: u32,

    /// Token shown by GET /api/runtime.
    #[arg(long, env = "WORDKEEP_RUNTIME_TOKEN")]
    token: String,

    /// Loopback wordkeep-wiki base URL.
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    wiki: String,

    /// Platform collector.
    #[arg(long, value_enum, default_value_t = Backend::Auto)]
    backend: Backend,

    /// Aggregate publish interval.
    #[arg(long, default_value_t = 500)]
    interval_ms: u64,

    /// Maximum live allocation records retained by the sampled tracker.
    #[arg(long, default_value_t = 100_000)]
    max_live: usize,

    /// Maximum distinct 4 KiB pages retained by the locality tracker.
    #[arg(long, default_value_t = 4_096)]
    max_hot_pages: usize,

    /// Replay a bpftrace / perf script / ETW text dump instead of a live collector.
    #[arg(long)]
    input: Option<std::path::PathBuf>,

    /// Keep reading `--input` as new lines arrive (tail).
    #[arg(long)]
    follow: bool,

    /// Print platform collector capabilities and exit.
    #[arg(long)]
    capabilities: bool,
}

#[derive(Clone, Copy)]
struct Allocation {
    size: u64,
    at: Instant,
}

#[derive(Clone)]
struct HotPage {
    samples: u64,
    weight: u64,
    misses: u64,
    data_source: String,
}

enum AllocEvent {
    Alloc(u64, u64),
    Free(u64),
    Realloc(u64, u64, u64),
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        eprintln!("wordkeep-runtime-helper: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    ensure_loopback(&cli.wiki)?;
    let backend = match cli.backend {
        Backend::Auto if cfg!(target_os = "linux") => Backend::Bpftrace,
        Backend::Auto if cfg!(windows) => Backend::Etw,
        Backend::Auto => return Err("no attach-stream backend for this platform".into()),
        explicit => explicit,
    };
    if cli.capabilities {
        println!(
            "{}",
            serde_json::to_string_pretty(&capabilities(backend)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if cli.input.is_some() || std::env::var_os("WORDKEEP_ETW_INPUT").is_some() {
        return run_input_file(&cli, backend);
    }
    match backend {
        Backend::Bpftrace => run_bpftrace(&cli),
        Backend::Perf => run_perf(&cli),
        Backend::Etw => run_etw(&cli),
        Backend::Auto => unreachable!(),
    }
}

fn ensure_loopback(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("wiki URL: {e}"))?;
    if parsed.scheme() != "http" {
        return Err("wiki URL must use http on loopback".into());
    }
    match parsed.host_str() {
        Some("127.0.0.1" | "localhost" | "::1") => Ok(()),
        _ => Err("wiki URL must be loopback".into()),
    }
}

fn command_exists(name: &str, arg: &str) -> bool {
    Command::new(name)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn capabilities(backend: Backend) -> Value {
    match backend {
        Backend::Bpftrace => json!({
            "backend": "bpftrace",
            "platform": std::env::consts::OS,
            "available": cfg!(target_os = "linux") && command_exists("bpftrace", "--version"),
            "quality": "sampled",
            "requires_elevation": true,
            "pre_attach_allocations": "unavailable"
        }),
        Backend::Perf => json!({
            "backend": "perf",
            "platform": std::env::consts::OS,
            "available": cfg!(target_os = "linux") && command_exists("perf", "--version"),
            "quality": "sampled",
            "requires_elevation": true,
            "data": "hardware memory-address samples"
        }),
        Backend::Etw => json!({
            "backend": "etw",
            "platform": std::env::consts::OS,
            "available": cfg!(windows) || std::env::var_os("WORDKEEP_ETW_INPUT").is_some(),
            "quality": if cfg!(windows) { "sampled" } else { "sampled" },
            "requires_elevation": cfg!(windows),
            "data": "heap alloc/free plus optional SampledProfile instruction addresses",
            "detail": if cfg!(windows) {
                "native Microsoft-Windows ETW real-time session; pre-attach allocations unavailable"
            } else {
                "replay --input / WORDKEEP_ETW_INPUT text exports on this host"
            }
        }),
        Backend::Auto => json!({"available": false}),
    }
}

#[cfg(target_os = "linux")]
fn run_bpftrace(cli: &Cli) -> Result<(), String> {
    if !command_exists("bpftrace", "--version") {
        return Err("bpftrace not found; install it or use cooperative diagnostics".into());
    }
    let script = r#"
uprobe:libc:malloc /pid == $PID/ { @wk_size[tid] = arg0; @wk_seen[tid] = 1; }
uretprobe:libc:malloc /pid == $PID && @wk_seen[tid]/ {
  printf("A %llu %llu\n", retval, @wk_size[tid]);
  delete(@wk_size[tid]); delete(@wk_seen[tid]);
}
uprobe:libc:calloc /pid == $PID/ { @wk_size[tid] = arg0 * arg1; @wk_seen[tid] = 1; }
uretprobe:libc:calloc /pid == $PID && @wk_seen[tid]/ {
  printf("A %llu %llu\n", retval, @wk_size[tid]);
  delete(@wk_size[tid]); delete(@wk_seen[tid]);
}
uprobe:libc:realloc /pid == $PID/ {
  @wk_old[tid] = arg0; @wk_size[tid] = arg1; @wk_seen[tid] = 1;
}
uretprobe:libc:realloc /pid == $PID && @wk_seen[tid]/ {
  printf("R %llu %llu %llu\n", @wk_old[tid], retval, @wk_size[tid]);
  delete(@wk_old[tid]); delete(@wk_size[tid]); delete(@wk_seen[tid]);
}
uprobe:libc:free /pid == $PID/ { printf("F %llu 0\n", arg0); }
"#
    .replace("$PID", &cli.pid.to_string());
    let mut child = Command::new("bpftrace")
        .arg("-q")
        .arg("-p")
        .arg(cli.pid.to_string())
        .arg("-e")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("start bpftrace: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "bpftrace stdout unavailable".to_string())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(event) = parse_event(&line) {
                if sender.send(event).is_err() {
                    break;
                }
            }
        }
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let interval = Duration::from_millis(cli.interval_ms.clamp(100, 10_000));
    let coverage_start_ns = unix_ns();
    let mut allocations = HashMap::new();
    let mut total_allocations = 0u64;
    let mut total_frees = 0u64;
    let mut live_bytes = 0u64;
    let mut peak_bytes = 0u64;
    let mut dropped = 0u64;
    let mut next_publish = Instant::now() + interval;
    loop {
        let timeout = next_publish.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(event) => {
                apply_event(
                    &mut allocations,
                    event,
                    cli.max_live,
                    &mut total_allocations,
                    &mut total_frees,
                    &mut live_bytes,
                    &mut peak_bytes,
                    &mut dropped,
                );
                while let Ok(event) = receiver.try_recv() {
                    apply_event(
                        &mut allocations,
                        event,
                        cli.max_live,
                        &mut total_allocations,
                        &mut total_frees,
                        &mut live_bytes,
                        &mut peak_bytes,
                        &mut dropped,
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = publish(
                    &client,
                    cli,
                    coverage_start_ns,
                    &allocations,
                    total_allocations,
                    total_frees,
                    peak_bytes,
                    dropped,
                );
                let status = child.wait().map_err(|e| e.to_string())?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("bpftrace exited with {status}"))
                };
            }
        }
        if Instant::now() >= next_publish {
            publish(
                &client,
                cli,
                coverage_start_ns,
                &allocations,
                total_allocations,
                total_frees,
                peak_bytes,
                dropped,
            )?;
            next_publish = Instant::now() + interval;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run_bpftrace(_cli: &Cli) -> Result<(), String> {
    Err("bpftrace backend is Linux-only".into())
}

#[cfg(target_os = "linux")]
fn run_perf(cli: &Cli) -> Result<(), String> {
    if !command_exists("perf", "--version") {
        return Err("perf not found; install linux perf or use cooperative diagnostics".into());
    }
    let mut recorder = Command::new("perf")
        .args(["mem", "record", "-q", "-p", &cli.pid.to_string(), "-o", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("start perf mem record: {e}"))?;
    let data = recorder
        .stdout
        .take()
        .ok_or_else(|| "perf data stream unavailable".to_string())?;
    let mut decoder = match Command::new("perf")
        .args(["script", "-i", "-", "-F", "addr,weight,data_src"])
        .stdin(Stdio::from(data))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = recorder.kill();
            return Err(format!("start perf script: {error}"));
        }
    };
    let stdout = decoder
        .stdout
        .take()
        .ok_or_else(|| "perf script stdout unavailable".to_string())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(sample) = parse_perf_sample(&line) {
                if sender.send(sample).is_err() {
                    break;
                }
            }
        }
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let coverage_start_ns = unix_ns();
    let interval = Duration::from_millis(cli.interval_ms.clamp(100, 10_000));
    let max_pages = cli.max_hot_pages.clamp(1, 65_536);
    let mut pages = HashMap::<u64, HotPage>::with_capacity(max_pages.min(4_096));
    let mut samples = 0u64;
    let mut dropped = 0u64;
    let mut next_publish = Instant::now() + interval;
    loop {
        let timeout = next_publish.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(sample) => {
                record_perf_sample(&mut pages, sample, max_pages, &mut samples, &mut dropped);
                while let Ok(sample) = receiver.try_recv() {
                    record_perf_sample(&mut pages, sample, max_pages, &mut samples, &mut dropped);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = publish_locality(
                    &client,
                    cli,
                    coverage_start_ns,
                    &pages,
                    samples,
                    dropped,
                    "linux-perf-mem",
                );
                let decoder_status = decoder.wait().map_err(|e| e.to_string())?;
                let _ = recorder.kill();
                let recorder_status = recorder.wait().map_err(|e| e.to_string())?;
                return if decoder_status.success() && recorder_status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "perf pipeline exited: record={recorder_status}, script={decoder_status}"
                    ))
                };
            }
        }
        if Instant::now() >= next_publish {
            publish_locality(
                &client,
                cli,
                coverage_start_ns,
                &pages,
                samples,
                dropped,
                "linux-perf-mem",
            )?;
            next_publish = Instant::now() + interval;
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run_perf(_cli: &Cli) -> Result<(), String> {
    Err("perf backend is Linux-only".into())
}

fn run_etw(cli: &Cli) -> Result<(), String> {
    #[cfg(windows)]
    {
        return run_etw_windows(cli);
    }
    #[cfg(not(windows))]
    {
        let _ = cli;
        Err(
            "native ETW needs Windows; pass --input with a tracerpt/xperf heap dump or WORDKEEP_ETW_INPUT"
                .into(),
        )
    }
}

fn input_path(cli: &Cli) -> Option<std::path::PathBuf> {
    cli.input
        .clone()
        .or_else(|| std::env::var_os("WORDKEEP_ETW_INPUT").map(std::path::PathBuf::from))
}

fn run_input_file(cli: &Cli, backend: Backend) -> Result<(), String> {
    let path = input_path(cli).ok_or_else(|| "no --input / WORDKEEP_ETW_INPUT".to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let coverage_start_ns = unix_ns();
    let max_live = cli.max_live.clamp(1, 1_000_000);
    let max_pages = cli.max_hot_pages.clamp(1, 65_536);
    let mut allocations = HashMap::<u64, Allocation>::new();
    let mut pages = HashMap::<u64, HotPage>::new();
    let (mut total_allocations, mut total_frees, mut live_bytes, mut peak_bytes, mut dropped) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut samples, mut sample_dropped) = (0u64, 0u64);
    let mut offset = 0usize;
    let locality_source = match backend {
        Backend::Perf => "linux-perf-mem",
        Backend::Etw => "etw-replay",
        _ => "file-replay",
    };

    loop {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if text.len() < offset {
            offset = 0;
        }
        for line in text[offset..].lines() {
            if let Some(event) = parse_event(line).or_else(|| parse_etw_heap_line(line)) {
                apply_event(
                    &mut allocations,
                    event,
                    max_live,
                    &mut total_allocations,
                    &mut total_frees,
                    &mut live_bytes,
                    &mut peak_bytes,
                    &mut dropped,
                );
            } else if let Some(sample) =
                parse_perf_sample(line).or_else(|| parse_etw_sample_line(line))
            {
                record_perf_sample(
                    &mut pages,
                    sample,
                    max_pages,
                    &mut samples,
                    &mut sample_dropped,
                );
            }
        }
        offset = text.len();
        if !allocations.is_empty() {
            publish(
                &client,
                cli,
                coverage_start_ns,
                &allocations,
                total_allocations,
                total_frees,
                peak_bytes,
                dropped,
            )?;
        }
        if !pages.is_empty() {
            publish_locality(
                &client,
                cli,
                coverage_start_ns,
                &pages,
                samples,
                sample_dropped,
                locality_source,
            )?;
        }
        if !cli.follow {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(cli.interval_ms.clamp(100, 10_000)));
    }
}

fn parse_hex_addr(raw: &str) -> Option<u64> {
    u64::from_str_radix(
        raw.trim().trim_start_matches("0x").trim_start_matches("0X"),
        16,
    )
    .ok()
}

fn field_map(line: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in line.split(|c: char| c == ',' || c.is_whitespace()) {
        if let Some((key, value)) = part.split_once('=') {
            out.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

/// tracerpt / xperf heap text: `Heap/Alloc addr=0x10 size=64` or `type=Alloc address=0x10 size=64`.
fn parse_etw_heap_line(line: &str) -> Option<AllocEvent> {
    let lower = line.to_ascii_lowercase();
    let fields = field_map(line);
    let addr = fields
        .get("addr")
        .or_else(|| fields.get("address"))
        .or_else(|| fields.get("ptr"))
        .and_then(|v| parse_hex_addr(v))?;
    let size = fields
        .get("size")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if lower.contains("realloc") {
        let new_addr = fields
            .get("newaddr")
            .or_else(|| fields.get("new_addr"))
            .and_then(|v| parse_hex_addr(v))
            .unwrap_or(addr);
        return Some(AllocEvent::Realloc(addr, new_addr, size));
    }
    if lower.contains("free") {
        return Some(AllocEvent::Free(addr));
    }
    if lower.contains("alloc") || lower.contains("allocation") {
        return Some(AllocEvent::Alloc(addr, size.max(1)));
    }
    None
}

/// ETW SampledProfile export: `SampledProfile addr=0x7ff0 weight=1` (instruction address).
fn parse_etw_sample_line(line: &str) -> Option<(u64, u64, String)> {
    let lower = line.to_ascii_lowercase();
    if !(lower.contains("sampledprofile") || (lower.contains("profile") && lower.contains("addr=")))
    {
        return None;
    }
    let fields = field_map(line);
    let address = fields
        .get("addr")
        .or_else(|| fields.get("address"))
        .or_else(|| fields.get("ip"))
        .and_then(|v| parse_hex_addr(v))?;
    if address == 0 {
        return None;
    }
    let weight = fields
        .get("weight")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1);
    Some((address, weight, "etw-ip".into()))
}

fn parse_perf_sample(line: &str) -> Option<(u64, u64, String)> {
    let mut fields = line.split_whitespace();
    let address = u64::from_str_radix(fields.next()?.trim_start_matches("0x"), 16).ok()?;
    if address == 0 {
        return None;
    }
    let weight = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    let data_source = fields.collect::<Vec<_>>().join(" ");
    Some((address, weight, data_source))
}

fn record_perf_sample(
    pages: &mut HashMap<u64, HotPage>,
    (address, weight, data_source): (u64, u64, String),
    max_pages: usize,
    samples: &mut u64,
    dropped: &mut u64,
) {
    *samples = samples.saturating_add(1);
    let page = address & !0xfffu64;
    if let Some(hot) = pages.get_mut(&page) {
        hot.samples = hot.samples.saturating_add(1);
        hot.weight = hot.weight.saturating_add(weight);
        hot.misses = hot
            .misses
            .saturating_add(u64::from(data_source.to_ascii_lowercase().contains("miss")));
        if !data_source.is_empty() {
            hot.data_source = data_source;
        }
    } else if pages.len() < max_pages {
        pages.insert(
            page,
            HotPage {
                samples: 1,
                weight,
                misses: u64::from(data_source.to_ascii_lowercase().contains("miss")),
                data_source,
            },
        );
    } else {
        *dropped = dropped.saturating_add(1);
    }
}

fn parse_event(line: &str) -> Option<AllocEvent> {
    let mut fields = line.split_whitespace();
    let kind = fields.next()?;
    let ptr = fields.next()?.parse::<u64>().ok()?;
    match kind {
        "A" => Some(AllocEvent::Alloc(ptr, fields.next()?.parse().ok()?)),
        "F" => Some(AllocEvent::Free(ptr)),
        "R" => Some(AllocEvent::Realloc(
            ptr,
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
        )),
        _ => None,
    }
}

fn apply_event(
    allocations: &mut HashMap<u64, Allocation>,
    event: AllocEvent,
    max_live: usize,
    total_allocations: &mut u64,
    total_frees: &mut u64,
    live_bytes: &mut u64,
    peak_bytes: &mut u64,
    dropped: &mut u64,
) {
    match event {
        AllocEvent::Alloc(ptr, size) if ptr != 0 => {
            if !allocations.contains_key(&ptr) && allocations.len() >= max_live {
                *dropped = dropped.saturating_add(1);
                return;
            }
            if let Some(old) = allocations.insert(
                ptr,
                Allocation {
                    size,
                    at: Instant::now(),
                },
            ) {
                *live_bytes = live_bytes.saturating_sub(old.size);
            }
            *live_bytes = live_bytes.saturating_add(size);
            *total_allocations = total_allocations.saturating_add(1);
        }
        AllocEvent::Free(ptr) if ptr != 0 => {
            if let Some(old) = allocations.remove(&ptr) {
                *live_bytes = live_bytes.saturating_sub(old.size);
            }
            *total_frees = total_frees.saturating_add(1);
        }
        AllocEvent::Realloc(old, new, size) => {
            if new != 0 {
                let replaced = if old != 0 {
                    allocations.remove(&old)
                } else {
                    None
                };
                if let Some(previous) = replaced {
                    *live_bytes = live_bytes.saturating_sub(previous.size);
                    *total_frees = total_frees.saturating_add(1);
                }
                if !allocations.contains_key(&new) && allocations.len() >= max_live {
                    *dropped = dropped.saturating_add(1);
                    return;
                }
                allocations.insert(
                    new,
                    Allocation {
                        size,
                        at: Instant::now(),
                    },
                );
                *live_bytes = live_bytes.saturating_add(size);
                *total_allocations = total_allocations.saturating_add(1);
            } else if size == 0 && old != 0 {
                if let Some(previous) = allocations.remove(&old) {
                    *live_bytes = live_bytes.saturating_sub(previous.size);
                }
                *total_frees = total_frees.saturating_add(1);
            }
        }
        _ => {}
    }
    *peak_bytes = (*peak_bytes).max(*live_bytes);
}

fn publish(
    client: &reqwest::blocking::Client,
    cli: &Cli,
    coverage_start_ns: u64,
    allocations: &HashMap<u64, Allocation>,
    total_allocations: u64,
    total_frees: u64,
    peak_bytes: u64,
    dropped: u64,
) -> Result<(), String> {
    let now = Instant::now();
    let mut buckets = [
        ("≤10 ms", 10u64, 0u64, 0u64),
        ("≤100 ms", 100, 0, 0),
        ("≤1 s", 1_000, 0, 0),
        ("≤10 s", 10_000, 0, 0),
        (">10 s", u64::MAX, 0, 0),
    ];
    let mut live_bytes = 0u64;
    let mut oldest_ms = 0u64;
    for allocation in allocations.values() {
        let age_ms = now
            .duration_since(allocation.at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        live_bytes = live_bytes.saturating_add(allocation.size);
        oldest_ms = oldest_ms.max(age_ms);
        if let Some(bucket) = buckets.iter_mut().find(|bucket| age_ms <= bucket.1) {
            bucket.2 = bucket.2.saturating_add(1);
            bucket.3 = bucket.3.saturating_add(allocation.size);
        }
    }
    let body = json!({
        "schema_version": 1,
        "kind": "runtime_overlay",
        "pid": cli.pid,
        "coverage_start_ns": coverage_start_ns,
        "capabilities": {"alloc_hooks": true},
        "quality": {"alloc_lifetime": "sampled"},
        "lifetime": {
            "quality": "sampled",
            "coverage_start_ns": coverage_start_ns,
            "pre_attach": "unavailable",
            "live_allocations": allocations.len(),
            "live_bytes": live_bytes,
            "peak_bytes": peak_bytes,
            "total_allocations": total_allocations,
            "total_frees": total_frees,
            "oldest_ms": oldest_ms,
            "age_buckets": buckets.map(|(label, _, count, bytes)| {
                json!({"label": label, "count": count, "bytes": bytes})
            })
        },
        "dropped": dropped,
        "estimated_overhead_pct": null
    });
    let response = client
        .post(format!(
            "{}/api/runtime/ingest",
            cli.wiki.trim_end_matches('/')
        ))
        .header("X-Wordkeep-Runtime", &cli.token)
        .json(&body)
        .send()
        .map_err(|e| format!("publish overlay: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("publish overlay: HTTP {}", response.status()));
    }
    Ok(())
}

fn publish_locality(
    client: &reqwest::blocking::Client,
    cli: &Cli,
    coverage_start_ns: u64,
    pages: &HashMap<u64, HotPage>,
    samples: u64,
    dropped: u64,
    source: &str,
) -> Result<(), String> {
    let mut ordered = pages.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .1
            .samples
            .cmp(&left.1.samples)
            .then_with(|| right.1.weight.cmp(&left.1.weight))
    });
    let hotspots = ordered
        .into_iter()
        .take(64)
        .map(|(address, hot)| {
            json!({
                "addr": format!("0x{address:016x}"),
                "end": format!("0x{:016x}", address.saturating_add(4_096)),
                "samples": hot.samples,
                "weight": hot.weight,
                "misses": hot.misses,
                "thread": "aggregated",
                "symbol": "unresolved",
                "data_source": &hot.data_source,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({
        "schema_version": 1,
        "kind": "runtime_overlay",
        "pid": cli.pid,
        "coverage_start_ns": coverage_start_ns,
        "capabilities": {"pmc": true},
        "quality": {"locality": "sampled"},
        "locality": {
            "quality": "sampled",
            "source": source,
            "address_kind": if source.contains("etw") { "instruction" } else { "data" },
            "coverage_start_ns": coverage_start_ns,
            "samples": samples,
            "dropped": dropped,
            "page_size": 4_096,
            "hotspots": hotspots
        }
    });
    let response = client
        .post(format!(
            "{}/api/runtime/ingest",
            cli.wiki.trim_end_matches('/')
        ))
        .header("X-Wordkeep-Runtime", &cli.token)
        .json(&body)
        .send()
        .map_err(|e| format!("publish locality overlay: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "publish locality overlay: HTTP {}",
            response.status()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn run_etw_windows(cli: &Cli) -> Result<(), String> {
    windows_etw::run(cli)
}

#[cfg(windows)]
mod windows_etw {
    use super::*;
    use std::ptr::null_mut;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
    use windows_sys::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW,
        EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_LOGFILEW,
        EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESS_TRACE_MODE_EVENT_RECORD,
        PROCESS_TRACE_MODE_REAL_TIME, TRACEHANDLE, WNODE_FLAG_TRACED_GUID,
    };

    const HEAP_PROVIDER: windows_sys::core::GUID = windows_sys::core::GUID {
        data1: 0x222962ab,
        data2: 0x6180,
        data3: 0x4b88,
        data4: [0xa8, 0x25, 0x34, 0x6b, 0x75, 0xf2, 0xa2, 0x4a],
    };
    const KERNEL_MEMORY: windows_sys::core::GUID = windows_sys::core::GUID {
        data1: 0xd1d93ef7,
        data2: 0xe1f2,
        data3: 0x4f45,
        data4: [0x86, 0x37, 0xeb, 0x31, 0x96, 0x89, 0xcb, 0x40],
    };

    struct Shared {
        pid: u32,
        tx: mpsc::Sender<Line>,
    }

    enum Line {
        Alloc(AllocEvent),
        Sample(u64, u64, String),
    }

    static SINK: Mutex<Option<Shared>> = Mutex::new(None);

    pub fn run(cli: &Cli) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        *SINK.lock().map_err(|e| e.to_string())? = Some(Shared {
            pid: cli.pid,
            tx: sender,
        });

        let mut name: Vec<u16> = "WordkeepRuntimeHeap\0".encode_utf16().collect();
        let props_bytes = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
        let mut storage = vec![0u8; props_bytes];
        let props = storage.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        unsafe {
            (*props).Wnode.BufferSize = props_bytes as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).BufferSize = 64;
            (*props).MinimumBuffers = 4;
            (*props).MaximumBuffers = 64;
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        }
        let mut handle: TRACEHANDLE = 0;
        let started = unsafe { StartTraceW(&mut handle, name.as_ptr(), props) };
        if started != ERROR_SUCCESS {
            return Err(format!(
                "StartTraceW failed ({started}); run the helper elevated for ETW heap"
            ));
        }
        for provider in [HEAP_PROVIDER, KERNEL_MEMORY] {
            let enable = unsafe {
                EnableTraceEx2(
                    handle,
                    &provider,
                    EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                    4,
                    0,
                    0,
                    0,
                    null_mut(),
                )
            };
            if enable != ERROR_SUCCESS {
                let _ = unsafe { ControlTraceW(handle, name.as_ptr(), props, 1) };
                return Err(format!("EnableTraceEx2 failed ({enable})"));
            }
        }

        let mut logfile = unsafe { std::mem::zeroed::<EVENT_TRACE_LOGFILEW>() };
        logfile.LoggerName = name.as_mut_ptr();
        logfile.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.EventRecordCallback = Some(on_event);
        let session = unsafe { OpenTraceW(&mut logfile) };
        if session == !0u64 {
            let _ = unsafe { ControlTraceW(handle, name.as_ptr(), props, 1) };
            return Err("OpenTraceW failed for real-time ETW session".into());
        }

        let consumer = std::thread::spawn(move || unsafe {
            ProcessTrace(&session, 1, null_mut(), null_mut())
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        let coverage_start_ns = unix_ns();
        let interval = Duration::from_millis(cli.interval_ms.clamp(100, 10_000));
        let max_live = cli.max_live.clamp(1, 1_000_000);
        let max_pages = cli.max_hot_pages.clamp(1, 65_536);
        let mut allocations = HashMap::<u64, Allocation>::new();
        let mut pages = HashMap::<u64, HotPage>::new();
        let (mut total_allocations, mut total_frees, mut live_bytes, mut peak_bytes, mut dropped) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        let (mut samples, mut sample_dropped) = (0u64, 0u64);
        let mut next_publish = Instant::now() + interval;

        let result = loop {
            let timeout = next_publish.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(timeout) {
                Ok(Line::Alloc(event)) => apply_event(
                    &mut allocations,
                    event,
                    max_live,
                    &mut total_allocations,
                    &mut total_frees,
                    &mut live_bytes,
                    &mut peak_bytes,
                    &mut dropped,
                ),
                Ok(Line::Sample(addr, weight, src)) => record_perf_sample(
                    &mut pages,
                    (addr, weight, src),
                    max_pages,
                    &mut samples,
                    &mut sample_dropped,
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
            }
            if Instant::now() >= next_publish {
                if let Err(error) = publish(
                    &client,
                    cli,
                    coverage_start_ns,
                    &allocations,
                    total_allocations,
                    total_frees,
                    peak_bytes,
                    dropped,
                ) {
                    break Err(error);
                }
                if !pages.is_empty() {
                    if let Err(error) = publish_locality(
                        &client,
                        cli,
                        coverage_start_ns,
                        &pages,
                        samples,
                        sample_dropped,
                        "etw-sampled-profile",
                    ) {
                        break Err(error);
                    }
                }
                next_publish = Instant::now() + interval;
            }
        };

        unsafe {
            let _ = CloseTrace(session);
            let _ = ControlTraceW(handle, name.as_ptr(), props, 1);
        }
        let _ = consumer.join();
        let _ = SINK.lock().map(|mut g| g.take());
        result
    }

    unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
        if record.is_null() {
            return;
        }
        let Ok(guard) = SINK.lock() else {
            return;
        };
        let Some(shared) = guard.as_ref() else {
            return;
        };
        let header = (*record).EventHeader;
        if header.ProcessId != 0 && header.ProcessId != shared.pid {
            return;
        }
        let user = std::slice::from_raw_parts(
            (*record).UserData as *const u8,
            (*record).UserDataLength as usize,
        );
        let opcode = header.EventDescriptor.Opcode;
        if let Some(event) = event_from_payload(opcode, user) {
            let _ = shared.tx.send(Line::Alloc(event));
        } else if let Some(addr) = sample_ip(user) {
            let _ = shared.tx.send(Line::Sample(addr, 1, "etw-ip".into()));
        }
        let _ = header;
    }

    fn event_from_payload(opcode: u8, user: &[u8]) -> Option<AllocEvent> {
        let addr = read_ptr(user, 0)?;
        let size = read_ptr(user, std::mem::size_of::<usize>()).unwrap_or(0);
        match opcode {
            0x20 | 0x21 | 32 | 33 => Some(AllocEvent::Alloc(addr, size.max(1))),
            0x22 | 34 => {
                let new_addr = read_ptr(user, std::mem::size_of::<usize>() * 2).unwrap_or(addr);
                Some(AllocEvent::Realloc(addr, new_addr, size.max(1)))
            }
            0x24 | 36 | 0x23 => Some(AllocEvent::Free(addr)),
            _ => None,
        }
    }

    fn sample_ip(user: &[u8]) -> Option<u64> {
        let addr = read_ptr(user, 0)?;
        (addr > 0x10000).then_some(addr)
    }

    fn read_ptr(user: &[u8], offset: usize) -> Option<u64> {
        if user.len() < offset + std::mem::size_of::<usize>() {
            return None;
        }
        let slice = &user[offset..offset + std::mem::size_of::<usize>()];
        Some(if cfg!(target_pointer_width = "64") {
            u64::from_le_bytes(slice.try_into().ok()?)
        } else {
            u32::from_le_bytes(slice.try_into().ok()?) as u64
        })
    }

    const _: WIN32_ERROR = ERROR_SUCCESS;
}

fn unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alloc_and_free_lines() {
        assert!(matches!(
            parse_event("A 4096 64"),
            Some(AllocEvent::Alloc(4096, 64))
        ));
        assert!(matches!(
            parse_event("F 4096 0"),
            Some(AllocEvent::Free(4096))
        ));
        assert!(matches!(
            parse_event("R 4096 8192 128"),
            Some(AllocEvent::Realloc(4096, 8192, 128))
        ));
        assert!(parse_event("Attaching 3 probes").is_none());
    }

    #[test]
    fn rejects_non_loopback_wiki() {
        assert!(ensure_loopback("http://127.0.0.1:8787").is_ok());
        assert!(ensure_loopback("https://example.com").is_err());
    }

    #[test]
    fn parses_and_bounds_perf_pages() {
        let sample = parse_perf_sample("0x0000000000001234 17 L3 miss").unwrap();
        assert_eq!(sample.0, 0x1234);
        assert_eq!(sample.1, 17);
        let mut pages = HashMap::new();
        let mut samples = 0;
        let mut dropped = 0;
        record_perf_sample(&mut pages, sample, 1, &mut samples, &mut dropped);
        record_perf_sample(
            &mut pages,
            (0x9000, 3, "L1 hit".into()),
            1,
            &mut samples,
            &mut dropped,
        );
        assert_eq!(pages.len(), 1);
        assert_eq!(samples, 2);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn allocation_tracker_is_bounded_and_keeps_live_bytes() {
        let mut allocations = HashMap::new();
        let (mut allocs, mut frees, mut live, mut peak, mut dropped) = (0, 0, 0, 0, 0);
        apply_event(
            &mut allocations,
            AllocEvent::Alloc(0x1000, 64),
            1,
            &mut allocs,
            &mut frees,
            &mut live,
            &mut peak,
            &mut dropped,
        );
        apply_event(
            &mut allocations,
            AllocEvent::Alloc(0x2000, 32),
            1,
            &mut allocs,
            &mut frees,
            &mut live,
            &mut peak,
            &mut dropped,
        );
        assert_eq!((allocations.len(), live, peak, dropped), (1, 64, 64, 1));
    }

    #[test]
    fn parses_etw_heap_and_sampled_profile_exports() {
        assert!(matches!(
            parse_etw_heap_line("Heap/Alloc addr=0x10 size=64"),
            Some(AllocEvent::Alloc(0x10, 64))
        ));
        assert!(matches!(
            parse_etw_heap_line("type=Free address=0x10"),
            Some(AllocEvent::Free(0x10))
        ));
        let sample = parse_etw_sample_line("SampledProfile addr=0x7ff01234 weight=3").unwrap();
        assert_eq!(sample.0, 0x7ff01234);
        assert_eq!(sample.1, 3);
        assert_eq!(sample.2, "etw-ip");
    }
}
