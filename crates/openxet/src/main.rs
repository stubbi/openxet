#![forbid(unsafe_code)]

mod cli;
mod output;

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(cli::run(std::env::args()))
}
