//! NUMA / DIMM topology from Linux sysfs and Windows GetNuma*.

use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub fn topology_json() -> Value {
    #[cfg(target_os = "linux")]
    {
        linux_topology()
    }
    #[cfg(windows)]
    {
        windows_topology()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        json!({
            "quality": "unavailable",
            "nodes": [],
            "detail": "NUMA topology backend unavailable on this platform"
        })
    }
}

#[cfg(target_os = "linux")]
fn linux_topology() -> Value {
    let base = Path::new("/sys/devices/system/node");
    if !base.is_dir() {
        return json!({
            "quality": "unavailable",
            "nodes": [],
            "detail": "no /sys/devices/system/node"
        });
    }
    let mut nodes = Vec::new();
    let dimms = dimm_slots();
    if let Ok(entries) = fs::read_dir(base) {
        let mut names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        names.sort_by_key(|e| e.file_name());
        for ent in names {
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("node") {
                continue;
            }
            let id: u32 = name.trim_start_matches("node").parse().unwrap_or(0);
            let dir = ent.path();
            let cpulist = read_trim(&dir.join("cpulist")).unwrap_or_default();
            let distance = read_trim(&dir.join("distance")).unwrap_or_default();
            let meminfo = read_trim(&dir.join("meminfo")).unwrap_or_default();
            let (mem_total, mem_free) = parse_node_meminfo(&meminfo);
            nodes.push(json!({
                "id": id,
                "cpulist": cpulist,
                "distance": distance,
                "mem_total_bytes": mem_total,
                "mem_free_bytes": mem_free,
                "dimms": [],
                "quality": {
                    "mem_total_bytes": if mem_total > 0 { "exact" } else { "unavailable" },
                    "dimms": "unavailable"
                }
            }));
        }
    }
    json!({
        "quality": if nodes.is_empty() { "unavailable" } else { "exact" },
        "dimm_quality": if dimms.is_empty() { "unavailable" } else { "exact" },
        "dimms": dimms,
        "nodes": nodes
    })
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_node_meminfo(text: &str) -> (u64, u64) {
    let mut total = 0u64;
    let mut free = 0u64;
    for line in text.lines() {
        // Node 0 MemTotal:       16384000 kB
        let lower = line.to_ascii_lowercase();
        if lower.contains("memtotal") {
            total = kb_from_line(line);
        } else if lower.contains("memfree") {
            free = kb_from_line(line);
        }
    }
    (total, free)
}

fn kb_from_line(line: &str) -> u64 {
    line.split_whitespace()
        .rev()
        .nth(1)
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(1024)
}

#[cfg(target_os = "linux")]
fn dimm_slots() -> Vec<Value> {
    let mut slots = Vec::new();
    // EDAC: /sys/devices/system/edac/mc/mc*/dimm*
    // EDAC usually does not expose a reliable DIMM→NUMA-node relation. Report
    // slots once at topology scope instead of duplicating every DIMM per node.
    let edac = Path::new("/sys/devices/system/edac/mc");
    if edac.is_dir() {
        if let Ok(mcs) = fs::read_dir(edac) {
            for mc in mcs.filter_map(|e| e.ok()) {
                if let Ok(children) = fs::read_dir(mc.path()) {
                    for dimm in children.filter_map(|e| e.ok()) {
                        let n = dimm.file_name();
                        let n = n.to_string_lossy();
                        if !n.starts_with("dimm") && !n.starts_with("rank") {
                            continue;
                        }
                        let label = read_trim(&dimm.path().join("dimm_label"))
                            .or_else(|| read_trim(&dimm.path().join("label")))
                            .unwrap_or_else(|| n.to_string());
                        let size = read_trim(&dimm.path().join("size"))
                            .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
                            .unwrap_or(0);
                        slots.push(json!({
                            "label": label,
                            "size_hint": size,
                            "memory_controller": mc.file_name().to_string_lossy(),
                        }));
                    }
                }
            }
        }
    }
    slots
}

#[cfg(windows)]
#[repr(C)]
struct GroupAffinity {
    mask: usize,
    group: u16,
    reserved: [u16; 3],
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetNumaHighestNodeNumber(highest_node_number: *mut u32) -> i32;
    fn GetNumaNodeProcessorMaskEx(node: u16, processor_mask: *mut GroupAffinity) -> i32;
    fn GetNumaAvailableMemoryNodeEx(node: u16, available_bytes: *mut u64) -> i32;
}

#[cfg(windows)]
fn windows_topology() -> Value {
    let mut highest = 0u32;
    // SAFETY: pointers reference initialized writable values for the duration
    // of each synchronous Kernel32 call.
    if unsafe { GetNumaHighestNodeNumber(&mut highest) } == 0 {
        return json!({
            "quality": "unavailable",
            "dimm_quality": "unavailable",
            "nodes": [],
            "dimms": [],
            "detail": "GetNumaHighestNodeNumber failed"
        });
    }
    let mut nodes = Vec::new();
    for id in 0..=highest.min(u16::MAX as u32) {
        let mut affinity = GroupAffinity {
            mask: 0,
            group: 0,
            reserved: [0; 3],
        };
        // Highest node ids may be sparse; failed ids are not topology nodes.
        if unsafe { GetNumaNodeProcessorMaskEx(id as u16, &mut affinity) } == 0 {
            continue;
        }
        let mut available = 0u64;
        let available_ok = unsafe { GetNumaAvailableMemoryNodeEx(id as u16, &mut available) } != 0;
        nodes.push(json!({
            "id": id,
            "cpulist": windows_cpu_list(affinity.group, affinity.mask),
            "processor_group": affinity.group,
            "processor_mask": format!("0x{:x}", affinity.mask),
            "distance": "",
            "mem_total_bytes": null,
            "mem_free_bytes": if available_ok { Some(available) } else { None },
            "dimms": [],
            "quality": {
                "cpulist": "exact",
                "mem_total_bytes": "unavailable",
                "mem_free_bytes": if available_ok { "exact" } else { "unavailable" },
                "distance": "unavailable",
                "dimms": "unavailable"
            }
        }));
    }
    json!({
        "quality": if nodes.is_empty() { "unavailable" } else { "exact" },
        "dimm_quality": "unavailable",
        "nodes": nodes,
        "dimms": [],
        "detail": "GetNuma* topology; Windows does not expose DIMM slots through this API"
    })
}

#[cfg(windows)]
fn windows_cpu_list(group: u16, mask: usize) -> String {
    let mut ranges = Vec::new();
    let mut bit = 0usize;
    while bit < usize::BITS as usize {
        if mask & (1usize << bit) == 0 {
            bit += 1;
            continue;
        }
        let start = bit;
        while bit + 1 < usize::BITS as usize && mask & (1usize << (bit + 1)) != 0 {
            bit += 1;
        }
        if start == bit {
            ranges.push(start.to_string());
        } else {
            ranges.push(format!("{start}-{bit}"));
        }
        bit += 1;
    }
    format!("g{group}:{}", ranges.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_meminfo_kb() {
        let t = "Node 0 MemTotal:       2048 kB\nNode 0 MemFree:        512 kB\n";
        let (t, f) = parse_node_meminfo(t);
        assert_eq!(t, 2048 * 1024);
        assert_eq!(f, 512 * 1024);
    }
}
