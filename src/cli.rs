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

    /// Print version information.
    Version,
}

/// Arguments for the `record` subcommand.
#[derive(clap::Args, Debug)]
pub struct RecordArgs {
    /// Output file path (default: wperf.data).
    #[arg(short, long, default_value = "wperf.data")]
    pub output: PathBuf,

    /// Recording duration in seconds (must be positive). If omitted, records until Ctrl+C.
    #[arg(short, long, value_parser = parse_positive_duration)]
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

    /// Spurious wakeup filter threshold in microseconds (§2.5).
    /// Wakeups where the thread runs for less than this before sleeping
    /// again are classified as noise. Set to 0 to disable filtering.
    #[arg(long, default_value = "50")]
    pub spurious_threshold_us: u32,
}

/// Output formats for `wperf report`.
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ReportFormat {
    Json,
    Dot,
    Svg,
}

/// Parse a positive duration value (rejects zero and negative).
fn parse_positive_duration(s: &str) -> Result<f64, String> {
    let val: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if val > 0.0 {
        Ok(val)
    } else {
        Err("duration must be positive".to_string())
    }
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
            _ => panic!("expected Record"),
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
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn parse_report() {
        let cli = Cli::parse_from(["wperf", "report", "trace.wperf"]);
        match cli.command {
            Command::Report(args) => {
                assert_eq!(args.input, PathBuf::from("trace.wperf"));
                assert_eq!(args.spurious_threshold_us, 50);
            }
            _ => panic!("expected Report"),
        }
    }

    #[test]
    fn parse_report_custom_spurious_threshold() {
        let cli = Cli::parse_from([
            "wperf",
            "report",
            "--spurious-threshold-us",
            "0",
            "trace.wperf",
        ]);
        match cli.command {
            Command::Report(args) => {
                assert_eq!(args.spurious_threshold_us, 0);
            }
            _ => panic!("expected Report"),
        }
    }

    #[test]
    fn record_short_output_flag() {
        let cli = Cli::parse_from(["wperf", "record", "-o", "out.wperf"]);
        match cli.command {
            Command::Record(args) => assert_eq!(args.output, PathBuf::from("out.wperf")),
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn parse_version_subcommand() {
        let cli = Cli::parse_from(["wperf", "version"]);
        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn record_rejects_zero_duration() {
        let result = Cli::try_parse_from(["wperf", "record", "-d", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn record_rejects_negative_duration() {
        let result = Cli::try_parse_from(["wperf", "record", "-d", "-1"]);
        assert!(result.is_err());
    }

    #[test]
    fn record_short_duration_flag() {
        let cli = Cli::parse_from(["wperf", "record", "-d", "5"]);
        match cli.command {
            Command::Record(args) => {
                assert!((args.duration.unwrap() - 5.0).abs() < f64::EPSILON);
            }
            _ => panic!("expected Record"),
        }
    }
}
