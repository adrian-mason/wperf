# wPerf

> **Status: Experimental / Pre-alpha — Not production-ready.**
> APIs, file format, and kernel compatibility surface may change during active development.

A Rust reimplementation of the [wPerf](https://www.usenix.org/conference/osdi18/presentation/yu) thread-level wait-for-graph profiler from OSDI'18.

wPerf traces scheduler events (`sched_switch`, `sched_wakeup`) via eBPF, builds a wait-for graph, and attributes blocking time through cascade redistribution — identifying which threads are truly responsible for end-to-end latency.

## Building

```bash
cargo build
cargo test
```

## License

This project is currently unlicensed. All rights reserved.
