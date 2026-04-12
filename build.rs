// SPDX-License-Identifier: Apache-2.0
//
// BPF skeleton generation via libbpf-cargo's SkeletonBuilder.
// Only active when compiled with `--features bpf`.
//
// Generates vmlinux.h from the running kernel's BTF (via bpftool),
// falling back to a vendored copy when bpftool is unavailable (CI).
// Compiles wperf.bpf.c into a skeleton with Rust bindings.
//
// Authoritative Inputs:
// - ADR-013 (dual-variant sched_switch + sched_wakeup probes)
// - ADR-004 (transport abstraction: ringbuf/perfarray)

fn main() {
    #[cfg(feature = "bpf")]
    skeleton_build();
}

#[cfg(feature = "bpf")]
fn skeleton_build() {
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));

    // vmlinux.h: try bpftool (exact kernel match), fall back to vendored copy.
    // Vendored copy at src/bpf/vmlinux.h.vendored (renamed to avoid clang
    // shadowing — #include "vmlinux.h" would find src/bpf/ before OUT_DIR).
    // BPF CO-RE relocations handle struct layout differences at load time.
    let vmlinux_h = out_dir.join("vmlinux.h");
    let generated = Command::new("bpftool")
        .args([
            "btf",
            "dump",
            "file",
            "/sys/kernel/btf/vmlinux",
            "format",
            "c",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success());

    if let Some(output) = generated.filter(|o| !o.stdout.is_empty()) {
        std::fs::write(&vmlinux_h, &output.stdout).expect("failed to write vmlinux.h");
    } else {
        std::fs::copy(manifest_dir.join("src/bpf/vmlinux.h.vendored"), &vmlinux_h)
            .expect("failed to copy vendored vmlinux.h — check src/bpf/vmlinux.h.vendored exists");
    }

    let src = "src/bpf/wperf.bpf.c";
    let skel_path = out_dir.join("wperf.skel.rs");

    libbpf_cargo::SkeletonBuilder::new()
        .source(src)
        .clang_args([
            "-I",
            out_dir.to_str().expect("OUT_DIR not UTF-8"),
            "-I",
            manifest_dir
                .join("src/bpf")
                .to_str()
                .expect("CARGO_MANIFEST_DIR not UTF-8"),
        ])
        .build_and_generate(&skel_path)
        .expect("failed to build and generate BPF skeleton");

    println!("cargo::rerun-if-changed={src}");
    println!("cargo::rerun-if-changed=src/bpf/wperf.h");
    println!("cargo::rerun-if-changed=src/bpf/compat.bpf.h");
    println!("cargo::rerun-if-changed=src/bpf/core_fixes.bpf.h");
    println!("cargo::rerun-if-changed=src/bpf/vmlinux.h.vendored");
}
