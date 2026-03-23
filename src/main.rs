// Author: Dustin Pilgrim
// License: GPL-3.0-only

mod app;
mod cli;
mod config;
mod core;
mod daemon;
mod ipc;
mod services;

use clap::Parser;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let args = cli::Args::parse();

    if args.command.is_some() {
        return app::command::run(args).await;
    }

    app::daemon_mode::run(args).await
}
