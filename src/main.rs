use std::net::TcpListener;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "mockreplay",
    about = "Record real HTTP traffic once, replay it for tests"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Proxy to a real (plain-HTTP) upstream, recording every exchange.
    Record {
        /// The real server to forward to, e.g. `localhost:9000`.
        #[arg(long)]
        target: String,
        #[arg(long, default_value = "127.0.0.1:8081")]
        listen: String,
        #[arg(long, default_value = "recording.json")]
        out: PathBuf,
    },
    /// Serve a previously recorded file, no network calls out.
    Replay {
        #[arg(long, default_value = "recording.json")]
        file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8081")]
        listen: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Record {
            target,
            listen,
            out,
        } => {
            let listener = TcpListener::bind(&listen)?;
            println!(
                "mockreplay recording: {listen} -> {target}  (writing {})",
                out.display()
            );
            mockreplay::record::serve(listener, target, out)?;
        }
        Command::Replay { file, listen } => {
            let content = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
            let recording: mockreplay::recording::Recording = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("parsing {}: {e}", file.display()))?;
            println!(
                "mockreplay replaying {} ({} recorded exchange(s)) on {listen}",
                file.display(),
                recording.entries.len()
            );
            let listener = TcpListener::bind(&listen)?;
            mockreplay::replay::serve(listener, recording)?;
        }
    }
    Ok(())
}
