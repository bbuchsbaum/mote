use clap::Parser;

use mote::cli::{Cli, near_miss_hint, run, run_help_all};
use mote::errors::MoteError;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut help_all = Vec::new();
    let mut json = false;
    for argument in args.iter().skip(1) {
        match argument.as_str() {
            "--json" => json = true,
            "--quiet" => {}
            other => help_all.push(other),
        }
    }
    if help_all == ["help", "--all"] {
        match run_help_all(json) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("mote: {error}");
                std::process::exit(3);
            }
        }
    }
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = error.exit_code();
            if let Err(print_error) = error.print() {
                eprintln!("mote: could not print command diagnostic: {print_error}");
            }
            if let Some(hint) = near_miss_hint(&args) {
                eprintln!("\nhint: {hint}");
            }
            std::process::exit(exit_code);
        }
    };
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("mote: {e}");
            let code = match e {
                MoteError::Rejected(_) => 2,
                MoteError::ActorUnresolved
                | MoteError::InvalidOpName(_)
                | MoteError::Invalid(_) => 3,
                MoteError::StoreNotFound(_)
                | MoteError::StoreAlreadyInitialized(_)
                | MoteError::HashMismatch { .. }
                | MoteError::DuplicateOp(_) => 4,
                _ => 1,
            };
            std::process::exit(code);
        }
    }
}
