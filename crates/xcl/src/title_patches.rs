//! Compatibility patches for known title binaries, made in memory after the title is mapped and
//! only where its bytes are exactly the expected ones: any other build is left as it is.
//!
//! The retail game's stall watchdog (`game_scarlett_master.exe` 1.0.2.1, the function at
//! +0x2215a60, decoded in #232) ends the process with a write to address 0 when one of its
//! watched threads has not posted for longer than a constant 300.0. On the console nothing
//! holds a thread that long; on a PC an NVIDIA driver takes up to 25 s for one big translated
//! compute pipeline at its first use, and a skipped dispatch leaves output the title reads (it
//! crashed on it). The watchdog's load of that limit (`vmovss xmm7, [rip + 300.0]`) is pointed
//! at a float.MAX the image already holds, so the limit is never reached; the 300.0 itself,
//! which other code may share, is not changed. `XCL_TITLE_WATCHDOG=1`: keep the watchdog.

use crate::modules::MappedModule;

/// `vmovss xmm7, dword [rip + disp32]` with the displacement the 1.0.2.1 build has
const WATCHDOG_LOAD_RVA: usize = 0x2215ae2;
const WATCHDOG_LOAD: [u8; 8] = [0xc5, 0xfa, 0x10, 0x3d, 0x36, 0x43, 0xf4, 0x03];
/// where that load reads: 300.0f
const WATCHDOG_LIMIT_RVA: usize = 0x6159e20;
const LIMIT_300: [u8; 4] = 300.0f32.to_le_bytes();
const FLOAT_MAX: [u8; 4] = f32::MAX.to_le_bytes();

pub unsafe fn apply(main: &MappedModule) {
    if std::env::var("XCL_TITLE_WATCHDOG").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    if !main.name.eq_ignore_ascii_case("game_scarlett_master.exe") {
        return;
    }
    match patch_watchdog(main.base, main.size) {
        Ok(at) => crate::xlog_info!("title patch: the stall watchdog's limit (+{WATCHDOG_LOAD_RVA:#x}) now reads float.MAX at +{at:#x} – long first-use pipeline builds stall the title instead of ending it (XCL_TITLE_WATCHDOG=1: keep it)"),
        Err(why) => crate::xlog_info!("title patch: stall watchdog left as it is ({why})"),
    }
}

/// The new rel32 for an instruction ending at `next` to reach `target`, if in range.
fn rel32(next: usize, target: usize) -> Option<i32> {
    i32::try_from(target as i64 - next as i64).ok()
}

/// Whether `code` is the expected load and `limit` the 300.0 it reads.
fn is_watchdog(code: &[u8], limit: &[u8]) -> bool {
    code == WATCHDOG_LOAD && limit == LIMIT_300
}

unsafe fn readable(addr: usize, len: usize) -> bool {
    crate::gpu::shader::readable(addr, len)
}

unsafe fn patch_watchdog(base: usize, size: usize) -> Result<usize, String> {
    let load = base + WATCHDOG_LOAD_RVA;
    let limit = base + WATCHDOG_LIMIT_RVA;
    if WATCHDOG_LIMIT_RVA + 4 > size || !readable(load, 8) || !readable(limit, 4) {
        return Err("not this build".into());
    }
    let code = std::slice::from_raw_parts(load as *const u8, 8);
    if !is_watchdog(code, std::slice::from_raw_parts(limit as *const u8, 4)) {
        return Err("not this build".into());
    }
    // a float.MAX in the image's read-only data (4-byte aligned), near the 300.0
    let mut found = None;
    let (lo, hi) = (WATCHDOG_LIMIT_RVA.saturating_sub(64 << 20) & !3, (WATCHDOG_LIMIT_RVA + (64 << 20)).min(size) & !3);
    let mut rva = lo;
    while rva + 4 <= hi {
        let a = base + rva;
        if (a & 0xFFF) == 0 && !readable(a, 0x1000.min(hi - rva)) {
            rva += 0x1000;
            continue;
        }
        if std::slice::from_raw_parts(a as *const u8, 4) == FLOAT_MAX {
            found = Some(rva);
            break;
        }
        rva += 4;
    }
    let target_rva = found.ok_or("no float.MAX in the image")?;
    let disp = rel32(load + 8, base + target_rva).ok_or("float.MAX out of reach")?;
    use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};
    let mut old = 0u32;
    if VirtualProtect((load + 4) as *const _, 4, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return Err("VirtualProtect failed".into());
    }
    std::ptr::write_unaligned((load + 4) as *mut i32, disp);
    let mut tmp = 0u32;
    VirtualProtect((load + 4) as *const _, 4, old, &mut tmp);
    windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache(windows_sys::Win32::System::Threading::GetCurrentProcess(), load as *const _, 8);
    Ok(target_rva)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the exact load of the exact 300.0 is the watchdog; a displacement reaches its target.
    #[test]
    fn the_watchdog_is_recognised_only_by_its_exact_bytes() {
        assert!(is_watchdog(&WATCHDOG_LOAD, &LIMIT_300));
        let mut other = WATCHDOG_LOAD;
        other[3] = 0x35; // xmm6: another load
        assert!(!is_watchdog(&other, &LIMIT_300));
        assert!(!is_watchdog(&WATCHDOG_LOAD, &200.0f32.to_le_bytes()));
        // the 1.0.2.1 displacement: +0x2215aea + 0x03f44336 = +0x6159e20
        assert_eq!(rel32(0x2215aea, 0x6159e20), Some(0x03f4_4336));
        assert_eq!(rel32(0x1000, 0x1_0000_1000), None, "out of rel32 range");
    }
}
