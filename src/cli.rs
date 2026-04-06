//! CLI definition for wperf — clap derive-based subcommands.
//!
//! Authoritative Input: final-design.md §1.2 (unified offline CLI model).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// wPerf — thread-level Wait-For-Graph performance analysis.
#[derive(Parser, Debug)]
#[command(name = "wperf", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Collect scheduling events via BPF probes into a .wperf file.
    Record(RecordArgs),

    /// Analyze a .wperf file and produce a performance report.
    Report(ReportArgs),
}

/// Arguments for the `record` subcommand.
#[derive(clap::Args, Debug)]
pub struct RecordArgs {
    /// Output file path (default: wperf.data).
    #[arg(short, long, default_value = "wperf.data")]
    pub output: PathBuf,

    /// Recording duration in seconds. If omitted, records until Ctrl+C.
    #[arg(short, long)]
    pub duration: Option<f64>,

    /// Transport buffer size in bytes.
    ///
    /// For ringbuf transport: sets `max_entries` (must be power of 2).
    /// For perfarray transport: sets per-CPU buffer size.
    /// Default: 16 MiB for ringbuf, 1 MiB/CPU for perfarray.
    #[arg(long)]
    pub buffer_size: Option<u32>,
}

/// Arguments for the `report` subcommand.
#[derive(clap::Args, Debug)]
pub struct ReportArgs {
    /// Input .wperf file to analyze.
    pub input: PathBuf,

    /// Output format.
    #[arg(short, long, default_value = "json")]
    pub format: ReportFormat,
}

/// Output formats for `wperf report`.
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ReportFormat {
    Json,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_record_defaults() {
        let cli = Cli::parse_from(["wperf", "record"]);
        match cli.command {
            Command::Record(args) => {
                assert_eq!(args.output, PathBuf::from("wperf.data"));
                assert!(args.duration.is_none());
                assert!(args.buffer_size.is_none());
            }
            Command::Report(_) => panic!("expected Record"),
        }
    }

    #[test]
    fn parse_record_all_args() {
        let cli = Cli::parse_from([
            "wperf",
            "record",
            "-o",
            "trace.wperf",
            "--duration",
            "10.5",
            "--buffer-size",
            "8388608",
        ]);
        match cli.command {
            Command::Record(args) => {
                assert_eq!(args.output, PathBuf::from("trace.wperf"));
                assert!((args.duration.unwrap() - 10.5).abs() < f64::EPSILON);
                assert_eq!(args.buffer_size.unwrap(), 8_388_608);
            }
            Command::Report(_) => panic!("expected Record"),
        }
    }

    #[test]
    fn parse_report() {
        let cli = Cli::parse_from(["wperf", "report", "trace.wperf"]);
        match cli.command {
            Command::Report(args) => {
                assert_eq!(args.input, PathBuf::from("trace.wperf"));
            }
            Command::Record(_) => panic!("expected Report"),
        }
    }

    #[test]
    fn record_short_output_flag() {
        let cli = Cli::parse_from(["wperf", "record", "-o", "out.wperf"]);
        match cli.command {
            Command::Record(args) => assert_eq!(args.output, PathBuf::from("out.wperf")),
            Command::Report(_) => panic!("expected Record"),
        }
    }

    #[test]
    fn record_short_duration_flag() {
        let cli = Cli::parse_from(["wperf", "record", "-d", "5"]);
        match cli.command {
            Command::Record(args) => {
                assert!((args.duration.unwrap() - 5.0).abs() < f64::EPSILON);
            }
            Command::Report(_) => panic!("expected Record"),
        }
    }
}
