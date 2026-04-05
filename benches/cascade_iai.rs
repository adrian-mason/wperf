//! iai-callgrind benchmarks for the cascade redistribution engine.
//!
//! Instruction-count benchmarks — deterministic, no noise from CI.
//! Requires valgrind + iai-callgrind-runner.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use wperf::cascade::engine::cascade_engine;
use wperf::graph::types::{NodeKind, ThreadId, TimeWindow};
use wperf::graph::wfg::WaitForGraph;

fn build_chain(n: i64) -> WaitForGraph {
    let mut g = WaitForGraph::new();
    for i in 0..n {
        g.add_node(ThreadId(i), NodeKind::UserThread);
    }
    for i in 0..n - 1 {
        g.add_edge(ThreadId(i), ThreadId(i + 1), TimeWindow::new(0, 100));
    }
    g
}

fn build_fan_out(n: i64) -> WaitForGraph {
    let mut g = WaitForGraph::new();
    for i in 0..n {
        g.add_node(ThreadId(i), NodeKind::UserThread);
    }
    for i in 1..n {
        g.add_edge(ThreadId(0), ThreadId(i), TimeWindow::new(0, 100));
    }
    g
}

fn setup_chain_16() -> WaitForGraph {
    build_chain(16)
}

fn setup_chain_64() -> WaitForGraph {
    build_chain(64)
}

fn setup_fan_out_16() -> WaitForGraph {
    build_fan_out(16)
}

fn setup_fan_out_64() -> WaitForGraph {
    build_fan_out(64)
}

#[library_benchmark]
#[bench::chain_16(setup_chain_16())]
#[bench::chain_64(setup_chain_64())]
fn bench_cascade_chain(g: WaitForGraph) {
    let _ = cascade_engine(&g, None);
}

#[library_benchmark]
#[bench::fan_out_16(setup_fan_out_16())]
#[bench::fan_out_64(setup_fan_out_64())]
fn bench_cascade_fan_out(g: WaitForGraph) {
    let _ = cascade_engine(&g, None);
}

library_benchmark_group!(
    name = cascade_iai;
    benchmarks = bench_cascade_chain, bench_cascade_fan_out
);

main!(library_benchmark_groups = cascade_iai);
