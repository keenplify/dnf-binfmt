mod cli;
mod desktop;
mod environment;
pub mod inspection;
mod native;
mod runtime;

pub use cli::Options;
pub use environment::{dnf_command, launch_command, CommandSpec, Profile};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn entry(args: Vec<String>) -> Result<u8> {
    if args.is_empty()
        || args
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == "--help" || arg == "-h")
    {
        println!("{}", cli::HELP);
        return Ok(0);
    }
    if args == ["--version"] {
        println!("dnf-binfmt {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    environment::execute(Options::parse(args)?)
}
