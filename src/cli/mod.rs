use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub mod commands;

#[derive(Debug, Parser)]
#[command(name = "symdex", version, about = "Local structural code indexer")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Index(IndexArgs),
    Status(StatusArgs),
    Symbols(SymbolsArgs),
    Refs(SymbolNameArgs),
    Callers(SymbolNameArgs),
    Callees(SymbolNameArgs),
    Imports(FileArgs),
    Errors(ErrorsArgs),
    Files(FilesArgs),
    Tui(StatusArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[arg(long)]
    pub full: bool,
    #[arg(long)]
    pub watch: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct SymbolNameArgs {
    pub symbol_name: String,
}

#[derive(Debug, Args)]
pub struct FileArgs {
    pub file: PathBuf,
}

#[derive(Debug, Args)]
pub struct ErrorsArgs {
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct FilesArgs {
    #[command(subcommand)]
    pub command: FilesCommand,
}

#[derive(Debug, Subcommand)]
pub enum FilesCommand {
    WithErrors,
}

#[derive(Debug, Args)]
pub struct SymbolsArgs {
    #[command(subcommand)]
    pub command: SymbolsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SymbolsCommand {
    Find(SymbolNameArgs),
    #[command(name = "in")]
    In(FileArgs),
}
