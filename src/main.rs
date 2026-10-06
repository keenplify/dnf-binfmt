use std::process::ExitCode;

fn main() -> ExitCode {
    match dnf_binfmt::entry(std::env::args().skip(1).collect()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("dnf-binfmt: {error}");
            ExitCode::FAILURE
        }
    }
}
