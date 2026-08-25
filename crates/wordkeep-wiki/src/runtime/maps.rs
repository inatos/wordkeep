//! Process virtual-address maps (Linux `/proc` and Windows `VirtualQueryEx`).

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub perm: String,
    pub kind: String,
    pub path: String,
    pub rss_bytes: u64,
}

impl Region {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Parse `/proc/<pid>/maps` (or a fixture). RSS is 0 unless filled from smaps.
pub fn parse_maps(text: &str) -> Vec<Region> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(6, char::is_whitespace);
        let range = parts.next().unwrap_or("");
        let perm = parts.next().unwrap_or("");
        let _offset = parts.next();
        let _dev = parts.next();
        let _inode = parts.next();
        let path = parts.next().unwrap_or("").trim();
        let mut range_parts = range.split('-');
        let start = u64::from_str_radix(range_parts.next().unwrap_or("0"), 16).unwrap_or(0);
        let end = u64::from_str_radix(range_parts.next().unwrap_or("0"), 16).unwrap_or(0);
        if end <= start {
            continue;
        }
        out.push(Region {
            start,
            end,
            perm: perm.to_string(),
            kind: classify_map(perm, path),
            path: path.to_string(),
            rss_bytes: 0,
        });
    }
    out
}

pub fn classify_map(perm: &str, path: &str) -> String {
    if path == "[heap]" {
        return "heap".into();
    }
    if path.starts_with("[stack") {
        return "stack".into();
    }
    if path.starts_with("[vdso") || path.starts_with("[vvar") || path.starts_with("[vsyscall") {
        return "vdso".into();
    }
    if path.ends_with(".so") || path.contains(".so.") {
        return "image".into();
    }
    if path.starts_with('/')
        && (perm.contains('x') || path.contains("/bin/") || path.ends_with(".exe"))
    {
        return "image".into();
    }
    if path.starts_with("/dev/") || path.starts_with("/anon") || path.is_empty() {
        if perm.contains('x') {
            return "anon_exec".into();
        }
        return "anon".into();
    }
    if path.starts_with('[') {
        return "kernel_helper".into();
    }
    "file".into()
}

pub fn committed_bytes(regions: &[Region]) -> u64 {
    regions.iter().map(Region::size).sum()
}

pub fn top_regions(regions: &[Region], n: usize) -> (Vec<Region>, u64, usize) {
    let mut indexed: Vec<Region> = regions.to_vec();
    indexed.sort_by_key(|r| std::cmp::Reverse(r.size()));
    let other_count = indexed.len().saturating_sub(n);
    let other_bytes: u64 = indexed.iter().skip(n).map(Region::size).sum();
    indexed.truncate(n);
    (indexed, other_bytes, other_count)
}

pub fn regions_json(regions: &[Region], other_bytes: u64, other_count: usize) -> Value {
    let mut arr: Vec<Value> = regions
        .iter()
        .map(|r| {
            json!({
                "start": format!("0x{:x}", r.start),
                "end": format!("0x{:x}", r.end),
                "start_u64": r.start,
                "end_u64": r.end,
                "size": r.size(),
                "perm": r.perm,
                "kind": r.kind,
                "path": if r.path.is_empty() { Value::Null } else { json!(r.path) },
                "rss_bytes": r.rss_bytes,
                "quality": { "size": "exact", "rss_bytes": if r.rss_bytes == 0 { "unavailable" } else { "exact" } }
            })
        })
        .collect();
    if other_count > 0 {
        arr.push(json!({
            "start": null,
            "end": null,
            "size": other_bytes,
            "kind": "other",
            "path": format!("{other_count} smaller regions"),
            "rss_bytes": 0,
            "quality": { "size": "exact" }
        }));
    }
    json!(arr)
}

/// Address-ordered scanline segments, including exact VA gaps.
///
/// The UI uses log(span) for display area so ASLR gaps do not erase committed
/// ranges. Segment order/start/end remain exact; visual area is deliberately
/// not presented as allocator fragmentation.
pub fn address_segments_json(regions: &[Region], max_segments: usize) -> Value {
    let mut ordered = regions.to_vec();
    ordered.sort_by_key(|region| region.start);
    let mut segments = Vec::with_capacity(ordered.len().saturating_mul(2));
    let mut previous_end = None;
    for region in ordered {
        if let Some(end) = previous_end {
            if region.start > end {
                segments.push(json!({
                    "start": format!("0x{end:x}"),
                    "end": format!("0x{:x}", region.start),
                    "size": region.start - end,
                    "kind": "gap",
                    "path": null,
                    "perm": "",
                    "quality": {"address": "exact", "display_area": "log_scaled"}
                }));
            }
        }
        segments.push(json!({
            "start": format!("0x{:x}", region.start),
            "end": format!("0x{:x}", region.end),
            "size": region.size(),
            "kind": region.kind,
            "path": if region.path.is_empty() { Value::Null } else { json!(region.path) },
            "perm": region.perm,
            "quality": {"address": "exact", "display_area": "log_scaled"}
        }));
        previous_end = Some(region.end);
    }
    if max_segments > 0 && segments.len() > max_segments {
        let source_count = segments.len();
        let group_size = source_count.div_ceil(max_segments);
        let mut grouped = Vec::with_capacity(max_segments);
        for chunk in segments.chunks(group_size) {
            let start = chunk.first().and_then(|v| v.get("start")).cloned();
            let end = chunk.last().and_then(|v| v.get("end")).cloned();
            let span_bytes: u64 = chunk
                .iter()
                .filter_map(|v| v.get("size").and_then(Value::as_u64))
                .sum();
            let first_kind = chunk
                .first()
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("mixed");
            let same_kind = chunk
                .iter()
                .all(|v| v.get("kind").and_then(Value::as_str) == Some(first_kind));
            grouped.push(json!({
                "start": start,
                "end": end,
                "size": span_bytes,
                "kind": if same_kind { first_kind } else { "mixed" },
                "path": format!("{} address segments", chunk.len()),
                "perm": "",
                "segment_count": chunk.len(),
                "quality": {"address": "exact endpoints", "display_area": "log_scaled", "contents": "aggregated"}
            }));
        }
        return json!({
            "projection": "scanline",
            "area_scale": "log2_span",
            "source_segments": source_count,
            "segments": grouped
        });
    }
    json!({
        "projection": "scanline",
        "area_scale": "log2_span",
        "source_segments": segments.len(),
        "segments": segments
    })
}

pub fn parse_status_kb(status: &str, key: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim().trim_start_matches(':').trim();
            let num = rest.split_whitespace().next()?;
            return num.parse::<u64>().ok().map(|kb| kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(target_os = "linux")]
pub fn process_name(pid: u32) -> Result<String, String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|_| {
            std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|name| name.trim().to_string())
        })
        .map_err(|e| format!("process identity: {e}"))
}

#[cfg(windows)]
pub fn process_name(pid: u32) -> Result<String, String> {
    type Handle = *mut std::ffi::c_void;
    type Bool = i32;
    type Dword = u32;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: Dword, inherit: Bool, pid: Dword) -> Handle;
        fn CloseHandle(handle: Handle) -> Bool;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: Dword,
            name: *mut u16,
            size: *mut Dword,
        ) -> Bool;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err("OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) failed".into());
        }
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as Dword;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) != 0;
        CloseHandle(handle);
        if !ok {
            return Err("QueryFullProcessImageNameW failed".into());
        }
        Ok(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn process_name(_pid: u32) -> Result<String, String> {
    Err("process identity unavailable on this OS".into())
}

#[cfg(target_os = "linux")]
pub fn read_pid_maps(pid: u32) -> Result<Vec<Region>, String> {
    let text =
        std::fs::read_to_string(format!("/proc/{pid}/maps")).map_err(|e| format!("maps: {e}"))?;
    Ok(parse_maps(&text))
}

#[cfg(target_os = "linux")]
pub fn read_pid_status(pid: u32) -> Result<(Option<u64>, Option<u64>, Option<u64>), String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|e| format!("status: {e}"))?;
    Ok((
        parse_status_kb(&text, "VmRSS"),
        parse_status_kb(&text, "VmHWM"),
        parse_status_kb(&text, "VmSwap"),
    ))
}

#[cfg(windows)]
pub fn read_pid_maps(_pid: u32) -> Result<Vec<Region>, String> {
    windows_maps(_pid)
}

#[cfg(windows)]
fn windows_maps(pid: u32) -> Result<Vec<Region>, String> {
    windows_virtual_query(pid)
}

#[cfg(windows)]
fn windows_virtual_query(pid: u32) -> Result<Vec<Region>, String> {
    use std::mem::{size_of, zeroed};
    type HANDLE = *mut std::ffi::c_void;
    type BOOL = i32;
    type DWORD = u32;
    type SIZE_T = usize;
    #[repr(C)]
    struct Mbi {
        base: usize,
        alloc_base: usize,
        alloc_protect: DWORD,
        size: SIZE_T,
        state: DWORD,
        protect: DWORD,
        type_: DWORD,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn CloseHandle(h: HANDLE) -> BOOL;
        fn VirtualQueryEx(
            h: HANDLE,
            addr: *const std::ffi::c_void,
            buf: *mut Mbi,
            len: SIZE_T,
        ) -> SIZE_T;
    }
    const QUERY: DWORD = 0x0400;
    const VM_READ: DWORD = 0x0010;
    const MEM_COMMIT: DWORD = 0x1000;
    const MEM_IMAGE: DWORD = 0x01000000;
    const MEM_MAPPED: DWORD = 0x00040000;
    unsafe {
        let h = OpenProcess(QUERY | VM_READ, 0, pid);
        if h.is_null() {
            return Err("OpenProcess failed".into());
        }
        let mut addr: usize = 0;
        let mut out = Vec::new();
        loop {
            let mut mbi: Mbi = zeroed();
            let got = VirtualQueryEx(h, addr as *const _, &mut mbi, size_of::<Mbi>());
            if got == 0 {
                break;
            }
            if mbi.state == MEM_COMMIT && mbi.size > 0 {
                let start = mbi.base as u64;
                let end = start.saturating_add(mbi.size as u64);
                out.push(Region {
                    start,
                    end,
                    perm: format!("{:x}", mbi.protect),
                    kind: if mbi.type_ == MEM_IMAGE {
                        "image"
                    } else if mbi.type_ == MEM_MAPPED {
                        "file"
                    } else {
                        "anon"
                    }
                    .into(),
                    path: String::new(),
                    rss_bytes: 0,
                });
            }
            let next = mbi.base.saturating_add(mbi.size);
            if next <= addr {
                break;
            }
            addr = next;
            if out.len() > 50_000 {
                break;
            }
        }
        CloseHandle(h);
        Ok(out)
    }
}

#[cfg(windows)]
pub fn read_pid_status(pid: u32) -> Result<(Option<u64>, Option<u64>, Option<u64>), String> {
    type Handle = *mut std::ffi::c_void;
    type Bool = i32;
    type Dword = u32;
    type SizeT = usize;
    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: Dword,
        page_fault_count: Dword,
        peak_working_set_size: SizeT,
        working_set_size: SizeT,
        quota_peak_paged_pool_usage: SizeT,
        quota_paged_pool_usage: SizeT,
        quota_peak_non_paged_pool_usage: SizeT,
        quota_non_paged_pool_usage: SizeT,
        pagefile_usage: SizeT,
        peak_pagefile_usage: SizeT,
        private_usage: SizeT,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: Dword, inherit: Bool, pid: Dword) -> Handle;
        fn CloseHandle(handle: Handle) -> Bool;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: Handle,
            counters: *mut ProcessMemoryCountersEx,
            size: Dword,
        ) -> Bool;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err("OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) failed".into());
        }
        let mut counters: ProcessMemoryCountersEx = std::mem::zeroed();
        counters.cb = std::mem::size_of::<ProcessMemoryCountersEx>() as Dword;
        let ok = GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0;
        CloseHandle(handle);
        if !ok {
            return Err("GetProcessMemoryInfo failed".into());
        }
        Ok((
            Some(counters.working_set_size as u64),
            Some(counters.peak_working_set_size as u64),
            None,
        ))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn read_pid_maps(_pid: u32) -> Result<Vec<Region>, String> {
    Err("maps unavailable on this OS".into())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn read_pid_status(_pid: u32) -> Result<(Option<u64>, Option<u64>, Option<u64>), String> {
    Err("status unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_fixture() {
        let maps = "\
00400000-0040c000 r-xp 00000000 00:00 0 /usr/bin/betwixt
00c00000-00e00000 rw-p 00000000 00:00 0 [heap]
7fff0000-7fff8000 rw-p 00000000 00:00 0 [stack]
";
        let r = parse_maps(maps);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].kind, "image");
        assert_eq!(r[1].kind, "heap");
        assert_eq!(r[1].size(), 0x200000);
        assert_eq!(committed_bytes(&r), 0xc000 + 0x200000 + 0x8000);
    }

    #[test]
    fn parse_status_rss() {
        let s = "Name:\tbetwixt\nVmRSS:\t  1234 kB\nVmHWM:\t  2000 kB\n";
        assert_eq!(parse_status_kb(s, "VmRSS"), Some(1234 * 1024));
        assert_eq!(parse_status_kb(s, "VmHWM"), Some(2000 * 1024));
    }

    #[test]
    fn address_segments_preserve_order_and_exact_gap() {
        let regions = vec![
            Region {
                start: 0x3000,
                end: 0x4000,
                perm: "rw-".into(),
                kind: "anon".into(),
                path: String::new(),
                rss_bytes: 0,
            },
            Region {
                start: 0x1000,
                end: 0x2000,
                perm: "r-x".into(),
                kind: "image".into(),
                path: "game".into(),
                rss_bytes: 0,
            },
        ];
        let map = address_segments_json(&regions, 16);
        assert_eq!(map["segments"][0]["start"], "0x1000");
        assert_eq!(map["segments"][1]["kind"], "gap");
        assert_eq!(map["segments"][1]["size"], 0x1000);
        assert_eq!(map["segments"][2]["start"], "0x3000");
    }
}
