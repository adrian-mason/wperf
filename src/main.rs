use std::process::ExitCode;

use clap::Parser;
use wperf::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        Command::Record(args) => wperf::record::run(&args).map_err(Into::into),
        Command::Report(args) => wperf::report::run(&args).map_err(Into::into),
        Command::Version => {
            println!("wperf {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wperf: {e}");
            ExitCode::FAILURE
        }
    }
}
