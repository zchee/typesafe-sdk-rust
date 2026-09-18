//! Spikes S2a, S2b and S4: one scenario per process run.
//!
//! Nothing here reads, sets or sends an API key. The live endpoint is contacted
//! only without an `Authorization` header, where it answers 403, which is all
//! these measurements need.

mod counting;
mod s2a;
mod s2b;
mod s4;
mod tls;

use std::{env, error::Error, process::ExitCode};

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let scenario = env::args().nth(1).unwrap_or_else(|| "help".to_owned());

    // A multi-thread runtime, because S2b's fan-out has to be genuinely
    // concurrent for the connection count to mean anything.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        match scenario.as_str() {
            "s2a" => s2a::run().await,
            "s2b" => s2b::run().await,
            "s4" => s4::run().await,
            other => {
                eprintln!("unknown scenario {other:?}");
                eprintln!("scenarios: s2a s2b s4");
                Ok(())
            }
        }
    })?;

    Ok(ExitCode::SUCCESS)
}
