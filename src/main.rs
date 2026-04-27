use clap::Parser;

use mote::cli::{Cli, run};
use mote::errors::MoteError;

fn main() {
    let cli = Cli::parse();
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
