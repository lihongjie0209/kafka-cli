//! Native Kafka command-line client implementation.

pub mod cli;
pub mod commands;
pub mod config;
pub mod dump_log;
pub mod error;
mod ffi;
pub mod output;
pub mod storage_tool;

use cli::Cli;
use error::Result;

/// Executes a parsed CLI invocation.
pub async fn run(cli: Cli) -> Result<()> {
    Box::pin(commands::execute(cli)).await
}
