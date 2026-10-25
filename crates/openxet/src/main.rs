#![forbid(unsafe_code)]

mod cli;
mod config;
mod config_cmd;
mod duration;
mod error;
mod host_memory;
mod label_cmd;
mod logging;
mod output;
mod preflight;
mod purge_cmd;
mod secret;
mod serve;
mod size;
mod token_cmd;
mod transfer_cmd;

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(cli::run(std::env::args_os()))
}
