//! Drydock command-line interface.

mod driver;
mod inspect;
mod launch;
mod player;
mod scenario;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "drydock", version, about = "Run, play and inspect original 32-bit Windows programs")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print PE headers, imports and hashes of an executable.
    Inspect {
        #[arg(long)]
        exe: std::path::PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Launch a program.
    Run(launch::RunArgs),
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Inspect { exe, json } => inspect::run(&exe, json),
        Cmd::Run(a) => match a.mode {
            launch::Mode::Play => player::run(a),
            launch::Mode::Headless => launch::run(a),
        },
    }
}
