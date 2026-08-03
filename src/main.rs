use std::process::ExitCode;

use kafka_cli::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse_compatible();
    match kafka_cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
