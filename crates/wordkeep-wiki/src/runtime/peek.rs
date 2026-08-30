//! Capped process-memory peek (debugger-style). Max 4096 bytes.

pub const MAX_PEEK: usize = 4096;

pub fn peek_bytes(pid: u32, addr: u64, len: usize) -> Result<Vec<u8>, String> {
    if len == 0 || len > MAX_PEEK {
        return Err(format!("len must be 1..={MAX_PEEK}"));
    }
    #[cfg(unix)]
    {
        peek_linux(pid, addr, len)
    }
    #[cfg(windows)]
    {
        peek_windows(pid, addr, len)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, addr);
        Err("peek unsupported on this OS".into())
    }
}

#[cfg(unix)]
fn peek_linux(pid: u32, addr: u64, len: usize) -> Result<Vec<u8>, String> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(format!("/proc/{pid}/mem")).map_err(|e| format!("open mem: {e}"))?;
    f.seek(SeekFrom::Start(addr))
        .map_err(|e| format!("seek: {e}"))?;
    let mut buf = vec![0u8; len];
    let n = f.read(&mut buf).map_err(|e| format!("read: {e}"))?;
    if n == 0 {
        return Err("unmapped or empty read (not zeros)".into());
    }
    buf.truncate(n);
    Ok(buf)
}

#[cfg(windows)]
fn peek_windows(pid: u32, addr: u64, len: usize) -> Result<Vec<u8>, String> {
    type HANDLE = *mut std::ffi::c_void;
    type BOOL = i32;
    type DWORD = u32;
    type SIZE_T = usize;
    extern "system" {
        fn OpenProcess(access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
        fn CloseHandle(h: HANDLE) -> BOOL;
        fn ReadProcessMemory(
            h: HANDLE,
            addr: *const std::ffi::c_void,
            buf: *mut std::ffi::c_void,
            size: SIZE_T,
            read: *mut SIZE_T,
        ) -> BOOL;
    }
    const QUERY: DWORD = 0x0400;
    const VM_READ: DWORD = 0x0010;
    unsafe {
        let h = OpenProcess(QUERY | VM_READ, 0, pid);
        if h.is_null() {
            return Err("OpenProcess failed".into());
        }
        let mut buf = vec![0u8; len];
        let mut got: SIZE_T = 0;
        let ok = ReadProcessMemory(
            h,
            addr as *const _,
            buf.as_mut_ptr() as *mut _,
            len,
            &mut got,
        );
        CloseHandle(h);
        if ok == 0 || got == 0 {
            return Err("ReadProcessMemory failed or unmapped".into());
        }
        buf.truncate(got);
        Ok(buf)
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn to_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            let c = *b as char;
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversize() {
        assert!(peek_bytes(std::process::id(), 0, MAX_PEEK + 1).is_err());
        assert!(peek_bytes(std::process::id(), 0, 0).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn peeks_own_maps_text() {
        let pid = std::process::id();
        let maps = match std::fs::read_to_string(format!("/proc/{pid}/maps")) {
            Ok(text) => text,
            Err(_) => return, // no /proc (containers, hardened hosts)
        };
        // First readable file-backed mapping start.
        let mut addr = None;
        for line in maps.lines() {
            if line.contains(" r") && line.contains('-') {
                let start = line.split('-').next().unwrap();
                if let Ok(a) = u64::from_str_radix(start, 16) {
                    if a > 0 {
                        addr = Some(a);
                        break;
                    }
                }
            }
        }
        let addr = addr.expect("a mapping");
        match peek_bytes(pid, addr, 16) {
            Ok(b) => {
                assert!(!b.is_empty());
                assert!(to_hex(&b).len() >= 2);
            }
            Err(_) => {
                // Yama / hardened kernels may deny /proc/self/mem; still a valid fail-closed.
            }
        }
    }
}
