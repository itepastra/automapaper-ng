use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(version, about)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    Set { name: String, value: String },
    DisplayShader { path: PathBuf },
    StateShader { path: PathBuf },
    Get { name: String },
}
