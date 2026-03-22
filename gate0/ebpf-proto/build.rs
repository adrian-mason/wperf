use libbpf_cargo::SkeletonBuilder;
use std::env;
use std::path::PathBuf;

const SRC: &str = "src/bpf/probe.bpf.c";

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("probe.skel.rs");
    SkeletonBuilder::new()
        .source(SRC)
        .clang_args([
            "-I",
            "src/bpf",
        ])
        .build_and_generate(&out)
        .unwrap();
    println!("cargo:rerun-if-changed={SRC}");
    println!("cargo:rerun-if-changed=src/bpf/vmlinux.h");
}
