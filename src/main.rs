use clap::Parser;

fn main() -> std::process::ExitCode {
    match wdb::app::run(wdb::cli::Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wdb: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
