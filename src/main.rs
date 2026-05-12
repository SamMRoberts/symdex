use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use symdex::cli::{Cli, commands};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    commands::run(cli)
}
