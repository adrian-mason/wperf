//! Criterion benchmarks for the cascade redistribution engine.
//!
//! Graphs: chain (linear), fan-out (star), and dense (complete).
//! Sizes chosen to cover typical (8-thread) and stress (64-thread) workloads.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use wperf::cascade::engine::cascade_engine;
use wperf::graph::types::{NodeKind, ThreadId, TimeWindow};
use wperf::graph::wfg::WaitForGraph;

/// Linear chain: T0 → T1 → T2 → ... → Tn
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

/// Fan-out: T0 → T1, T0 → T2, ..., T0 → Tn
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

/// Dense: every thread waits for every other (complete graph).
fn build_dense(n: i64) -> WaitForGraph {
    let mut g = WaitForGraph::new();
    for i in 0..n {
        g.add_node(ThreadId(i), NodeKind::UserThread);
    }
    for i in 0..n {
        for j in 0..n {
            if i != j {
                g.add_edge(ThreadId(i), ThreadId(j), TimeWindow::new(0, 100));
            }
        }
    }
    g
}

fn bench_cascade(c: &mut Criterion) {
    let mut group = c.benchmark_group("cascade_engine");

    for &n in &[8, 16, 32, 64] {
        let chain = build_chain(n);
        group.bench_with_input(BenchmarkId::new("chain", n), &chain, |b, g| {
            b.iter(|| cascade_engine(g, None).unwrap());
        });

        let fan = build_fan_out(n);
        group.bench_with_input(BenchmarkId::new("fan_out", n), &fan, |b, g| {
            b.iter(|| cascade_engine(g, None).unwrap());
        });
    }

    // Dense graphs are expensive — complete graph causes exponential path explosion
    // in cascade recursion. Keep n small to avoid multi-minute runtimes.
    for &n in &[4, 8] {
        let dense = build_dense(n);
        group.bench_with_input(BenchmarkId::new("dense", n), &dense, |b, g| {
            b.iter(|| cascade_engine(g, None).unwrap());
        });
    }

    group.finish();
}

criterion_group!(benches, bench_cascade);
criterion_main!(benches);
