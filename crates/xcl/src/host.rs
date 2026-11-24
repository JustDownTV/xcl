//! The host process: started by the launcher with the game's command line. Sets up
//! logging, the VFS, hooks and HLE, maps the title and jumps to its entry point.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Boot;

pub static HOST_MODE: AtomicBool = AtomicBool::new(false);

pub fn host_main(boot_path: &Path) -> i32 {
    let boot: Boot = match std::fs::read_to_string(boot_path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("xcl host: cannot read {}: {e}", boot_path.display());
            return 0x7C1_0001;
        }
    };
    crate::log::init(
        Some(&boot.logs_dir().join("xcl.log")),
        crate::log::Level::parse(&boot.config.log_level),
        boot.config.log_to_stderr,
    );
    crate::xlog_info!("XCL {} – host for '{}'", env!("CARGO_PKG_VERSION"), boot.title);
    crate::xlog_info!("game: {} (read-only folder: {})", boot.exe_path.display(), boot.game_root.display());
    crate::xlog_info!("XCL data: {}", boot.data_dir.display());

    if std::env::var_os("XCL_UMA_MB").is_none() && boot.config.uma_mb != 0 {
        std::env::set_var("XCL_UMA_MB", boot.config.uma_mb.to_string());
    }
    crate::state::set_boot(boot.clone());
    // `XCL_ACTIVITY=1`: every 2 s what the title did since (a timeline of where it stops)
    if std::env::var_os("XCL_ACTIVITY").is_some() {
        let _ = std::thread::Builder::new().name("xcl-activity".into()).spawn(|| {
            let snap = || {
                let (p, e) = crate::hle::d3d12x::present::activity();
                let (d, x) = crate::hle::d3d12x::bind::activity();
                let f = crate::hle::d3d12x::frame::activity();
                let (r, b, fail) = crate::hle::dstorage::counters();
                let pso = &crate::hle::d3d12x::pso_async::waited_ms;
                let lost = crate::hle::d3d12x::pso_async::standin_draws() as u64;
                [p, e, d, x, f[0], f[1], f[2], f[3], r, b >> 20, fail, pso(), lost]
            };
            let mut last = snap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let now = snap();
                let d: Vec<u64> = now.iter().zip(last).map(|(a, b)| a - b).collect();
                crate::xlog_info!(
                    "activity (2 s): {} presents, {} submits, {} draws, {} dispatches, {} frame-event waits ({} timed out), {} frame statistics ({} not flipped); DirectStorage {} requests, {} MB, {} failed; pipeline waits {} ms, {} draws lost to pipelines still building",
                    d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12]
                );
                // where the memory is: the process's commit, video memory, and XCL's parts of it
                let private = unsafe {
                    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
                    let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
                    c.cb = std::mem::size_of_val(&c) as u32;
                    K32GetProcessMemoryInfo(windows_sys::Win32::System::Threading::GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb);
                    c.PrivateUsage
                };
                let (local, non_local) = crate::hle::d3d12x::real::video_memory().unwrap_or((0, 0));
                let (arenas, arenas_made) = crate::hle::d3d12x::vram::arena_bytes();
                let (pools, pools_mapped) = crate::hle::d3d12x::sparse::pool_bytes();
                let (pages, page_pools) = crate::hle::d3d12x::vram::page_bytes();
                let mb = |b: u64| b >> 20;
                crate::xlog_info!(
                    "memory: {} MB committed by the process; video memory {} MB local, {} MB non-local; target pages {} MB (pools {} MB); arenas {} MB in use, {} MB made; sparse pools {} MB ({} MB mapped)",
                    mb(private as u64),
                    mb(local),
                    mb(non_local),
                    mb(pages as u64),
                    mb(page_pools as u64),
                    mb(arenas as u64),
                    mb(arenas_made as u64),
                    mb(pools as u64),
                    mb(pools_mapped as u64)
                );
                let (evicted, mapped_again) = crate::hle::d3d12x::vram::eviction_counts();
                if evicted > 0 {
                    crate::xlog_info!("page budget: {evicted} textures evicted, {mapped_again} mapped again");
                }
                last = now;
            }
        });
    }
    // `XCL_MEMORY_MAP=s`: every s seconds, where the process's committed address space is
    if let Some(every) = std::env::var("XCL_MEMORY_MAP").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|&s| s > 0) {
        let _ = std::thread::Builder::new().name("xcl-memory-map".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(every));
            log_memory_map();
        });
    }
    // `XCL_THREAD_DUMP_AT=s1,s2…`: where every thread is at those seconds (a stalled title)
    if let Ok(v) = std::env::var("XCL_THREAD_DUMP_AT") {
        let at: Vec<f64> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let _ = std::thread::Builder::new().name("xcl-thread-dump".into()).spawn(move || {
            let t0 = std::time::Instant::now();
            for s in at {
                let left = s - t0.elapsed().as_secs_f64();
                if left > 0.0 {
                    std::thread::sleep(std::time::Duration::from_secs_f64(left));
                }
                crate::eh::dump_threads();
            }
        });
    }
    // `XCL_RUN_SECONDS=n`: the title ends itself after n seconds – the GPU drained, the log and
    // report written (a process killed from outside with GPU work in flight can hang in the driver)
    if let Some(secs) = std::env::var("XCL_RUN_SECONDS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let (shown, presented) = crate::hle::d3d12x::present::counters();
            crate::xlog_info!("XCL_RUN_SECONDS: {secs} s – ending the title ({shown} frames shown, {presented} PresentX calls)");
            crate::report::write_final(None);
            crate::log::flush();
            crate::hle::d3d12x::real::end_title(0);
        });
    }
    crate::stubs::init_db(&boot.config.stub_returns);
    crate::hle::init(&boot.config);
    crate::hle::kernelx::fine_timer();
    crate::loader::set_cache_dir(boot.cache_dir());
    let mut dirs = vec![boot.game_root.clone()];
    if let Some(p) = boot.exe_path.parent() {
        if p != boot.game_root {
            dirs.insert(0, p.to_path_buf());
        }
    }
    crate::resolver::set_search_dirs(dirs);

    let mounts: Vec<(String, String)> = boot.config.mounts.iter().map(|(k, v)| (k.clone(), boot.expand(v))).collect();
    // (the shader cache is XCL's too: on a drive a console mount names it must stay where it is)
    crate::vfs::init(crate::vfs::Vfs::new(&boot.game_root, &boot.data_dir, mounts).own_folder(&crate::gpu::recomp::dxc::cache_dir()));
    let _ = std::fs::create_dir_all(boot.data_dir.join("temp"));

    if let Err(e) = crate::vfs_hooks::install() {
        crate::xlog_error!("VFS: not all hooks installed: {e}");
    }
    if let Err(e) = crate::eh::install_pc_hook() {
        crate::xlog_error!("hook RtlPcToFileHeader: {e} – C++ exceptions in the game may not work");
    }
    crate::eh::install_crash_handler();
    crate::eh::start_probes();
    HOST_MODE.store(true, Ordering::Release);

    let main = match crate::loader::load_main(&boot.exe_path) {
        Ok(m) => m,
        Err(e) => {
            crate::xlog_error!("loading the game failed: {e}");
            crate::report::write_final(Some(format!("loader: {e}")));
            return 0x7C1_0002;
        }
    };
    unsafe { crate::hle::kernelx::adopt_title_hooks(&main) };
    unsafe { crate::title_patches::apply(&main) };
    crate::report::write_final(None);
    let code = crate::loader::run_main(&main);
    crate::xlog_info!("game entry point returned code {code}");
    crate::report::write_final(None);
    crate::log::flush();
    code as i32
}

/// `XCL_MEMORY_MAP`: the committed address space by kind (private / mapped / image), private
/// allocations by size, and the allocations holding the most. Video memory WDDM charges to the
/// process is not in the address space: the `memory:` line's total minus these is that part.
fn log_memory_map() {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED};
    let mut by_base: std::collections::HashMap<usize, (u64, u64, u32)> = std::collections::HashMap::new();
    // committed private bytes by page protection (write-combined: memory the GPU shares)
    let mut by_protect: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let (mut private, mut mapped, mut image) = (0u64, 0u64, 0u64);
    let mut addr = 0usize;
    loop {
        let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { VirtualQuery(addr as *const _, &mut m, std::mem::size_of_val(&m)) } == 0 {
            break;
        }
        let size = m.RegionSize as u64;
        if m.State == MEM_COMMIT {
            match m.Type {
                MEM_IMAGE => image += size,
                MEM_MAPPED => mapped += size,
                _ => {
                    private += size;
                    *by_protect.entry(m.Protect).or_default() += size;
                }
            }
            let e = by_base.entry(m.AllocationBase as usize).or_insert((0, 0, m.Type));
            e.0 += size;
        }
        if let Some(e) = by_base.get_mut(&(m.AllocationBase as usize)) {
            e.1 = e.1.max((m.BaseAddress as usize + m.RegionSize - m.AllocationBase as usize) as u64);
        }
        match (m.BaseAddress as usize).checked_add(m.RegionSize) {
            Some(next) if next > addr => addr = next,
            _ => break,
        }
    }
    let mb = |b: u64| b >> 20;
    // private allocations by how much each holds: under 1 MB, 1 - 64 MB, 64 MB and more
    let mut classes = [(0u64, 0u64); 3];
    for &(c, _, ty) in by_base.values() {
        if ty != MEM_IMAGE && ty != MEM_MAPPED && c > 0 {
            let k = if c < 1 << 20 { 0 } else if c < 64 << 20 { 1 } else { 2 };
            classes[k].0 += 1;
            classes[k].1 += c;
        }
    }
    crate::xlog_info!(
        "memory map: {} MB private, {} MB mapped, {} MB image committed; private allocations: {} under 1 MB ({} MB), {} of 1-64 MB ({} MB), {} of 64 MB+ ({} MB)",
        mb(private),
        mb(mapped),
        mb(image),
        classes[0].0,
        mb(classes[0].1),
        classes[1].0,
        mb(classes[1].1),
        classes[2].0,
        mb(classes[2].1)
    );
    // the 1 - 64 MB private allocations by committed size (MB, rounded up): which sizes repeat
    let mut sizes: std::collections::BTreeMap<u64, (u64, u64)> = std::collections::BTreeMap::new();
    for &(c, span, ty) in by_base.values() {
        if ty != MEM_IMAGE && ty != MEM_MAPPED && (1 << 20..64 << 20).contains(&c) {
            let e = sizes.entry(c.div_ceil(1 << 20)).or_default();
            e.0 += 1;
            e.1 += span;
        }
    }
    let mut sizes: Vec<(u64, (u64, u64))> = sizes.into_iter().collect();
    sizes.sort_by(|a, b| (b.1 .0 * b.0).cmp(&(a.1 .0 * a.0)));
    crate::xlog_info!(
        "memory map: private by protection: {}; 1-64 MB allocations by size: {}",
        by_protect.iter().map(|(p, b)| format!("{p:#x} {} MB", mb(*b))).collect::<Vec<_>>().join(", "),
        sizes.iter().take(10).map(|(m, (n, span))| format!("{n}x {m} MB (reserved {} MB each)", mb(span / n))).collect::<Vec<_>>().join(", ")
    );
    let mut top: Vec<(usize, (u64, u64, u32))> = by_base.into_iter().filter(|(_, (c, _, _))| *c > 0).collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let line: Vec<String> = top
        .iter()
        .take(12)
        .map(|(base, (c, span, ty))| format!("{base:#x} {} of {} MB{}", mb(*c), mb(*span), match *ty { MEM_MAPPED => " mapped", MEM_IMAGE => " image", _ => "" }))
        .collect();
    crate::xlog_info!("memory map: largest committed allocations: {}", line.join(", "));
}
