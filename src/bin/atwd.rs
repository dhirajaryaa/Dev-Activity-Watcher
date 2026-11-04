use std::process::ExitCode;

// The daemon entry point. Behaves exactly like `atw run`; kept as a separate
// binary so the systemd unit reads clearly and `ps`/`htop` show "atwd".
fn main() -> ExitCode {
    atw::cli_main(std::env::args_os())
}
