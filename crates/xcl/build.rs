use std::path::Path;

// `pso.rs` also contains cache storage and pipeline setup. Keep it out of the source hash so
// those changes do not discard translations; bump this only when its container parsing,
// translation options, profiles, or post-translation HLSL changes.
const TRANSLATION_GLUE_REVISION: &str = "pso-glue-v1";

const TRANSLATOR_SOURCES: &[&str] = &[
    "src/gpu/dxbc.rs",
    "src/gpu/rdna2.rs",
    "src/gpu/rdna2_spec.rs",
    "src/gpu/rdna2_xbox.rs",
    "src/gpu/recomp/dxc.rs",
    "src/gpu/recomp/emit.rs",
    "src/gpu/recomp/mesh_wave.rs",
    "src/gpu/recomp/mod.rs",
    "src/gpu/recomp/upper_lanes.rs",
    "src/gpu/recomp/x64_masks.rs",
    "src/gpu/shader.rs",
    "src/gpu/xbox.rs",
];

fn add(h: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *h ^= byte as u64;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

fn main() {
    // xcl.exe without high-entropy ASLR: the title's memory then lies under 2^40, which a
    // retail title's BVH builder requires of its GPU addresses (#585, PR #631)
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bin=xcl=/HIGHENTROPYVA:NO"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bin=xcl=-Wl,--disable-high-entropy-va"),
        _ => {}
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    add(&mut h, b"xcl-translator-source-v1\0");
    add(&mut h, TRANSLATION_GLUE_REVISION.as_bytes());
    add(&mut h, &[0]);
    for relative in TRANSLATOR_SOURCES {
        println!("cargo:rerun-if-changed={relative}");
        add(&mut h, relative.as_bytes());
        add(&mut h, &[0]);
        add(&mut h, &std::fs::read(root.join(relative)).expect("read shader translator source"));
        add(&mut h, &[0]);
    }
    println!("cargo:rustc-env=XCL_TRANSLATOR_SOURCE_ID={h:016x}");

    // xcl.exe's process heap is a segment heap: the driver's shader compiler allocates from it
    // on every thread, and on the NT heap its threads queued on the heap lock – 0.4 of a core
    // busy for 8 pipeline compiles, 11 cores with the segment heap. Only the heap type: the
    // common controls stay in the menu's own activation context (`main.rs`).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("xcl.manifest");
        std::fs::write(&manifest, SEGMENT_HEAP_MANIFEST).expect("write the manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
}

const SEGMENT_HEAP_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <heapType xmlns="http://schemas.microsoft.com/SMI/2020/WindowsSettings">SegmentHeap</heapType>
    </windowsSettings>
  </application>
</assembly>
"#;
