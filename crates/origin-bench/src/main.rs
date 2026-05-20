use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "origin-bench", about = "Benchmark origin vs CC / jcode / opencode")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List the task set without running anything.
    List,
    /// Run origin against the task set.
    RunOrigin {
        #[arg(long)]
        tasks: std::path::PathBuf,
    },
    /// Run a comparison contestant via subprocess.
    RunSubprocess {
        #[arg(long)]
        name: String,
        #[arg(long)]
        bin: std::path::PathBuf,
        #[arg(long)]
        tasks: std::path::PathBuf,
    },
    /// Render the comparison report.
    Report {
        #[arg(long)]
        results: std::path::PathBuf,
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => println!("(task list will be populated in P14.C.3)"),
        Cmd::RunOrigin { tasks: _ } => println!("(origin runner lands in P14.C.4)"),
        Cmd::RunSubprocess {
            name,
            bin: _,
            tasks: _,
        } => {
            println!("(subprocess runner for {name} lands in P14.C.5)");
        }
        Cmd::Report { results: _, out } => {
            std::fs::write(out, "# Bench report\n_pending implementation._\n")?;
        }
    }
    Ok(())
}
