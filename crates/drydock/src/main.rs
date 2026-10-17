//! Drydock command-line interface.

mod driver;
mod inspect;
mod launch;
mod player;
mod scenario;
mod serve;

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
    /// Serve the JSON control protocol.
    Serve {
        /// Exchange line-delimited JSON over stdin/stdout.
        #[arg(long)]
        stdio: bool,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    let cli = Cli::parse();
    // Emulation is latency-bound work on this thread; ask macOS for performance cores.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
    match cli.cmd {
        Cmd::Inspect { exe, json } => inspect::run(&exe, json),
        Cmd::Serve { stdio } => {
            if !stdio {
                anyhow::bail!("only --stdio is supported");
            }
            serve::serve_stdio()
        }
        Cmd::Run(a) => match a.mode {
            launch::Mode::Play => player::run(a),
            launch::Mode::Headless => launch::run(a),
        },
    }
}
