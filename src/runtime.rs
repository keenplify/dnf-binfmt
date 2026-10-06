use crate::{
    environment::{launch_command, CommandSpec, Profile},
    inspection::Inspection,
    Options, Result,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn app_ids(root: &Path, executable: &str) -> Result<Vec<String>> {
    let directory = root.join("usr/share/applications");
    if !directory.exists() {
        return Ok(vec![]);
    }
    let name = Path::new(executable).file_name().unwrap_or_default();
    let mut ids = vec![];
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|v| v != "desktop")
        {
            continue;
        }
        let content = fs::read_to_string(entry.path())?;
        let mut main = false;
        let mut matches = false;
        let mut identity = None;
        for line in content.lines() {
            if line.starts_with('[') {
                main = line == "[Desktop Entry]";
            }
            if !main {
                continue;
            }
            if let Some(exec) = line.strip_prefix("Exec=") {
                let first = exec
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches('"');
                matches = Path::new(first).file_name().is_some_and(|v| v == name);
            }
            for key in ["X-DBUS-ServiceName=", "StartupWMClass="] {
                if let Some(value) = line.strip_prefix(key) {
                    if valid_identity(value) {
                        identity = Some(value.to_string());
                    }
                }
            }
        }
        if matches {
            if let Some(identity) = identity {
                ids.push(identity);
            }
            let filename = entry.path();
            if let Some(stem) = filename.file_stem().and_then(|v| v.to_str()) {
                if valid_identity(stem) {
                    ids.push(stem.into());
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn valid_identity(value: &str) -> bool {
    value.contains('.')
        && value.split('.').all(|part| {
            !part.is_empty()
                && !part.as_bytes()[0].is_ascii_digit()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
}

fn bridge_helper() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let installed = executable
        .parent()
        .ok_or("backend has no parent")?
        .join("dnf-binfmt-session-bus");
    if installed.is_file() {
        return Ok(installed);
    }
    // A Cargo development build can use the helper in its source repository.
    if let Some(target) = executable
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "target"))
    {
        let source = target
            .parent()
            .ok_or("target directory has no parent")?
            .join("helpers/session_bus.py");
        if source.is_file() {
            return Ok(source);
        }
    }
    Err("session-bus helper is missing; reinstall dnf-binfmt or use --session-bus off".into())
}

pub fn plan(
    profile: &Profile,
    image: &Path,
    options: &Options,
    capabilities: &Inspection,
) -> Result<CommandSpec> {
    let software = match options.graphics.as_str() {
        "software" => true,
        "accelerated" => false,
        _ => capabilities.gui(),
    };
    let bus = match options.session_bus.as_str() {
        "filtered" => true,
        "off" => false,
        _ => capabilities.session_bus(),
    };
    let mut command = launch_command(profile, image, &options.args);
    let mut environment = vec![];
    if capabilities.gui() || software {
        environment.extend(["GDK_BACKEND=x11", "NO_AT_BRIDGE=1", "GTK_MODULES="]);
    }
    if software {
        environment.extend([
            "LIBGL_ALWAYS_SOFTWARE=1",
            "GALLIUM_DRIVER=llvmpipe",
            "LIBGL_DRI3_DISABLE=1",
        ]);
    }
    let args = environment
        .into_iter()
        .flat_map(|value| ["-e".to_string(), value.to_string()])
        .collect::<Vec<_>>();
    command.args.splice(0..0, args);
    if !bus {
        // Prevent an inherited host UNIX bus path from being used accidentally.
        command
            .args
            .splice(0..0, ["-e".into(), "DBUS_SESSION_BUS_ADDRESS=".into()]);
        return Ok(command);
    }
    let mut args = vec![bridge_helper()?.to_string_lossy().into_owned()];
    if let Some(generation) = image.parent() {
        for identity in app_ids(&generation.join("root"), &options.args[0])? {
            args.extend(["--app-id".into(), identity]);
        }
    }
    args.extend(["--".into(), command.program]);
    args.extend(command.args);
    Ok(CommandSpec::new("/usr/bin/python3", args))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_gui_policy_enables_software_rendering_and_filtered_bridge() {
        let profile = Profile {
            directory: "/var/lib/dnf-binfmt/default".into(),
            releasever: "44".into(),
            overlays: vec![],
        };
        let mut options =
            Options::parse(vec!["run".into(), "--".into(), "example-app".into()]).unwrap();
        let mut capabilities = Inspection::default();
        capabilities.needed.insert("libgtk-3.so.0".into());
        let command = plan(
            &profile,
            Path::new("/imaginary/rootfs.erofs"),
            &options,
            &capabilities,
        )
        .unwrap();
        assert_eq!(command.program, "/usr/bin/python3");
        assert!(command.args.contains(&"LIBGL_ALWAYS_SOFTWARE=1".into()));
        assert!(command.args.contains(&"LIBGL_DRI3_DISABLE=1".into()));
        options.graphics = "accelerated".into();
        options.session_bus = "off".into();
        let command = plan(
            &profile,
            Path::new("/imaginary/rootfs.erofs"),
            &options,
            &capabilities,
        )
        .unwrap();
        assert_eq!(command.program, "/usr/bin/muvm");
        assert!(!command.args.contains(&"LIBGL_ALWAYS_SOFTWARE=1".into()));
        assert!(command.args.contains(&"DBUS_SESSION_BUS_ADDRESS=".into()));
    }

    #[test]
    fn desktop_identity_is_inferred_without_wildcard_ownership() {
        let root = std::env::temp_dir().join(format!(
            "dnf-binfmt-identity-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("usr/share/applications")).unwrap();
        fs::write(
            root.join("usr/share/applications/io.example.Auth.desktop"),
            "[Desktop Entry]\nExec=auth-app %U\nStartupWMClass=io.example.Auth\n",
        )
        .unwrap();
        assert_eq!(
            app_ids(&root, "/usr/bin/auth-app").unwrap(),
            ["io.example.Auth"]
        );
        assert!(app_ids(&root, "unrelated-app").unwrap().is_empty());
        assert!(!valid_identity("org.example.*"));
        fs::remove_dir_all(root).unwrap();
    }
}
