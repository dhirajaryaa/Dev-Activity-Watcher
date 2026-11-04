use std::process::ExitCode;

fn main() -> ExitCode {
    atw::cli_main(std::env::args_os())
}
