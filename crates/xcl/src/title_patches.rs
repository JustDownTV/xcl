//! Compatibility patches for known title binaries, made in memory after the title is mapped and
//! only where its bytes are exactly the expected ones: any other build is left as it is. The
//! game file on disk is never changed.
//!
//! The retail game's stall watchdog (`game_scarlett_master.exe` 1.0.2.1, decoded in #232) is the
//! entry function of a thread of its own (+0x2215a60; a generic thread start calls it through
//! `call [rsi]` at +0x2215ec8). It ends the process – the report hook it calls writes to address
//! 0 (+0x23cf683), then it writes there itself (+0x2215c35) – when a watched thread has not
//! posted for too long. On the console nothing holds a thread that long; on a PC an NVIDIA
//! driver takes 9 - 25 s for one big translated compute pipeline at its first use, and the
//! title was ended there every run (skipping those dispatches instead left output it read and
//! crashed on). Pointing the limit's load at float.MAX did not stop it, so the thread itself is
//! parked: the entry jumps to `park`, which sleeps for good. `XCL_TITLE_WATCHDOG=1`: keep it.

use crate::modules::MappedModule;

/// the watchdog thread's entry and its first 16 bytes in the 1.0.2.1 build
/// (push r15 / r14 / r13 / r12 / rsi / rdi / rbp / rbx; sub rsp, 0x68)
const WATCHDOG_ENTRY_RVA: usize = 0x2215a60;
const WATCHDOG_ENTRY: [u8; 16] = [0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x56, 0x57, 0x55, 0x53, 0x48, 0x83, 0xec, 0x68];

pub unsafe fn apply(main: &MappedModule) {
    if std::env::var("XCL_TITLE_WATCHDOG").is_ok_and(|v| v.trim() == "1") {
        return;
    }
    if !main.name.eq_ignore_ascii_case("game_scarlett_master.exe") {
        return;
    }
    match park_watchdog(main.base, main.size) {
        Ok(()) => crate::xlog_info!("title patch: the stall watchdog thread (+{WATCHDOG_ENTRY_RVA:#x}) is parked – a long first-use pipeline build stalls the title instead of ending it (XCL_TITLE_WATCHDOG=1: keep it)"),
        Err(why) => crate::xlog_info!("title patch: stall watchdog left as it is ({why})"),
    }
}

/// `mov rax, target; jmp rax`
fn jump_to(target: usize) -> [u8; 12] {
    let mut c = [0u8; 12];
    c[0] = 0x48;
    c[1] = 0xB8;
    c[2..10].copy_from_slice(&(target as u64).to_le_bytes());
    c[10] = 0xFF;
    c[11] = 0xE0;
    c
}

/// Where the watchdog thread goes instead: it sleeps for good (the process ends it).
unsafe extern "system" fn park() -> ! {
    crate::xlog_info!("title patch: the stall watchdog thread started and is parked");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

unsafe fn park_watchdog(base: usize, size: usize) -> Result<(), String> {
    let entry = base + WATCHDOG_ENTRY_RVA;
    if WATCHDOG_ENTRY_RVA + 16 > size || !crate::gpu::shader::readable(entry, 16) {
        return Err("not this build".into());
    }
    if std::slice::from_raw_parts(entry as *const u8, 16) != WATCHDOG_ENTRY {
        return Err("not this build".into());
    }
    let code = jump_to(park as usize);
    use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_EXECUTE_READWRITE};
    let mut old = 0u32;
    if VirtualProtect(entry as *const _, code.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return Err("VirtualProtect failed".into());
    }
    std::ptr::copy_nonoverlapping(code.as_ptr(), entry as *mut u8, code.len());
    let mut tmp = 0u32;
    VirtualProtect(entry as *const _, code.len(), old, &mut tmp);
    windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache(windows_sys::Win32::System::Threading::GetCurrentProcess(), entry as *const _, code.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The jump is `mov rax, imm64; jmp rax` to the given address, 12 bytes (inside the 16 checked).
    #[test]
    fn the_entry_jump_reaches_its_target() {
        let c = jump_to(0x1122_3344_5566_7788);
        assert_eq!(c[..2], [0x48, 0xB8]);
        assert_eq!(u64::from_le_bytes(c[2..10].try_into().unwrap()), 0x1122_3344_5566_7788);
        assert_eq!(c[10..], [0xFF, 0xE0]);
        assert!(c.len() <= WATCHDOG_ENTRY.len());
    }

    /// A function whose first bytes differ is left alone.
    #[test]
    fn another_build_is_left_alone() {
        let mut image = vec![0u8; WATCHDOG_ENTRY_RVA + 16];
        image[WATCHDOG_ENTRY_RVA..].copy_from_slice(&[0x90; 16]);
        let r = unsafe { park_watchdog(image.as_ptr() as usize, image.len()) };
        assert_eq!(r, Err("not this build".to_string()));
        assert!(image[WATCHDOG_ENTRY_RVA..].iter().all(|&b| b == 0x90), "nothing written");
    }
}
