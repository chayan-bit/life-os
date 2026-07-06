//! `lifeos` - the allow-listed CLI. Talks to lifeos-api over localhost only,
//! never the DB directly. Exit codes: 0 ok, 2 usage/local, 3 API error,
//! 4 connection error, 5 parse error.

mod cli;
mod client;
mod commands;
mod config;
mod output;
#[cfg(test)]
mod test_support;

use clap::Parser;
use cli::{Cli, Commands};
use client::Client;
use output::Output;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args = Cli::parse();
    let out = Output { json: args.json };

    // `config` is local-only and never needs the API.
    if let Commands::Config { cmd } = args.command {
        return finish(commands::misc::config(out, cmd));
    }

    let settings = config::resolve(args.api_url, args.token, args.workspace);
    let client = Client::new(settings);

    let result = match args.command {
        Commands::Config { .. } => unreachable!("handled above"),
        Commands::Status => commands::misc::status(&client, out).await,
        Commands::Metrics => commands::misc::metrics(&client, out).await,
        Commands::Entity { cmd } => commands::data::entity(&client, out, cmd).await,
        Commands::Edge { cmd } => commands::data::edge(&client, out, cmd).await,
        Commands::Event { cmd } => commands::data::event(&client, out, cmd).await,
        Commands::Job { cmd } => commands::data::job(&client, out, cmd).await,
        Commands::Gmail { cmd } => commands::integrations::gmail(&client, out, cmd).await,
        Commands::Calendar { cmd } => commands::integrations::calendar(&client, out, cmd).await,
        Commands::Drive { cmd } => commands::integrations::drive(&client, out, cmd).await,
        Commands::Notion { cmd } => commands::integrations::notion(&client, out, cmd).await,
        Commands::Slack { cmd } => commands::integrations::slack(&client, out, cmd).await,
        Commands::File { cmd } => commands::misc::file(&client, out, cmd).await,
    };
    finish(result)
}

fn finish(result: Result<(), client::CliError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e.message());
            ExitCode::from(e.exit_code() as u8)
        }
    }
}
