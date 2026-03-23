use libbpf_cargo::SkeletonBuilder;
use std::env;
use std::path::PathBuf;

fn build_skel(src: &str, out_name: &str) {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join(out_name);
    SkeletonBuilder::new()
        .source(src)
        .clang_args(["-I", "src/bpf"])
        .build_and_generate(&out)
        .unwrap();
    println!("cargo:rerun-if-changed={src}");
}

fn main() {
    build_skel("src/bpf/probe.bpf.c", "probe.skel.rs");
    build_skel("src/bpf/probe_rb.bpf.c", "probe_rb.skel.rs");
    println!("cargo:rerun-if-changed=src/bpf/vmlinux.h");
}
