use crate::Result;
use std::path::{Component, Path, PathBuf};

pub const HELP: &str = "Experimental x86-64 RPM environments for Fedora Asahi Remix.
Usage: dnf binfmt COMMAND [OPTIONS] [ARGUMENTS...]
       dnf-binfmt COMMAND [OPTIONS] [ARGUMENTS...]

Commands:
  init       Create a profile (requires --releasever; run with sudo)
  install    Install packages or local RPMs (run with sudo)
  upgrade    Upgrade the profile's packages (run with sudo)
  remove     Remove profile packages (run with sudo)
  list       List installed packages
  run        Launch a command through muvm + FEXBash (without sudo)
  export     Generate this user's application-menu entries (without sudo)
  doctor     Check host requirements
  inspect    Inspect a local RPM's ELF libraries and runtime capabilities

Options:
  --profile NAME          Profile name (default: default)
  --state-dir PATH        Absolute state directory (default: /var/lib/dnf-binfmt)
  --releasever NUMBER     Fedora release for init, e.g. 44
  --overlay PATH          Extra EROFS image for init; repeatable (e.g. GPU libraries)
  --accept-no-scripts     Compatibility option; scripts/triggers are always skipped
  --dry-run               Print commands without executing or writing files
  --graphics MODE         Run: auto, software, accelerated (default: auto)
  --session-bus MODE      Run: auto, filtered, off (default: auto)
  --                      Pass remaining arguments literally (particularly for run)

RPM scripts and triggers are NOT executed in this experimental version.
Applications needing them may not work. DNF4 is not supported.";

#[derive(Debug, Clone)]
pub struct Options {
    pub command: String,
    pub profile: String,
    pub state_dir: PathBuf,
    pub releasever: Option<String>,
    pub overlays: Vec<PathBuf>,
    pub accept_no_scripts: bool,
    pub dry_run: bool,
    pub args: Vec<String>,
    pub graphics: String,
    pub session_bus: String,
}

pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
        && !path.to_string_lossy().contains(['\n', '\r'])
}

impl Options {
    pub fn parse(args: Vec<String>) -> Result<Self> {
        let mut result = Self {
            command: args.first().ok_or("a command is required")?.clone(),
            profile: "default".into(),
            state_dir: "/var/lib/dnf-binfmt".into(),
            releasever: None,
            overlays: vec![],
            accept_no_scripts: false,
            dry_run: false,
            args: vec![],
            graphics: "auto".into(),
            session_bus: "auto".into(),
        };
        let mut iter = args.into_iter().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--" => {
                    result.args.extend(iter);
                    break;
                }
                "--profile" => result.profile = iter.next().ok_or("--profile requires a name")?,
                "--state-dir" => {
                    result.state_dir = iter.next().ok_or("--state-dir requires a path")?.into()
                }
                "--releasever" => {
                    result.releasever = Some(iter.next().ok_or("--releasever requires a number")?)
                }
                "--overlay" => result
                    .overlays
                    .push(iter.next().ok_or("--overlay requires a path")?.into()),
                "--accept-no-scripts" => result.accept_no_scripts = true,
                "--dry-run" => result.dry_run = true,
                "--graphics" => {
                    result.graphics = iter.next().ok_or("--graphics requires a mode")?
                }
                "--session-bus" => {
                    result.session_bus = iter.next().ok_or("--session-bus requires a mode")?
                }
                _ if arg.starts_with('-') => {
                    return Err(format!(
                        "unknown option {arg}; use -- before application arguments"
                    )
                    .into())
                }
                _ => result.args.push(arg),
            }
        }
        if !valid_name(&result.profile) {
            return Err("invalid profile name".into());
        }
        if !matches!(
            result.graphics.as_str(),
            "auto" | "software" | "accelerated"
        ) || !matches!(result.session_bus.as_str(), "auto" | "filtered" | "off")
        {
            return Err("invalid graphics or session-bus mode".into());
        }
        if result.command != "run" && (result.graphics != "auto" || result.session_bus != "auto") {
            return Err("graphics and session-bus options are only supported by run".into());
        }
        if !safe_absolute(&result.state_dir) {
            return Err("state directory must be absolute, normalized, and not /".into());
        }
        if let Some(release) = &result.releasever {
            if release.is_empty() || !release.bytes().all(|c| c.is_ascii_digit()) {
                return Err("releasever must be a Fedora release number".into());
            }
        }
        for path in &result.overlays {
            if !safe_absolute(path) {
                return Err("overlay paths must be absolute and normalized".into());
            }
        }
        if result.command != "init" && (result.releasever.is_some() || !result.overlays.is_empty())
        {
            return Err("releasever and overlay are only supported by init".into());
        }
        match result.command.as_str() {
            "init" | "list" | "export" | "doctor" if !result.args.is_empty() => {
                return Err("this command does not accept positional arguments".into())
            }
            "install" | "remove" | "run" if result.args.is_empty() => {
                return Err("at least one argument is required".into())
            }
            "inspect" if result.args.len() != 1 => {
                return Err("inspect requires one local RPM path".into())
            }
            "init" | "install" | "upgrade" | "remove" | "run" | "list" | "export" | "doctor"
            | "inspect" => (),
            _ => return Err(format!("unknown command {}", result.command).into()),
        }
        if result.command != "run" && result.args.iter().any(|a| a.starts_with('-')) {
            return Err("package arguments cannot be options".into());
        }
        Ok(result)
    }
}
