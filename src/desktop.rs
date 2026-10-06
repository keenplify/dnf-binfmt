use crate::{environment::Profile, Options, Result};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

// Desktop Exec syntax is distinct from shell syntax. Quote every backend
// argument and double percent signs so field-code expansion cannot alter them.
pub fn exec_quote(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\\\\\"),
            '"' | '$' | '`' => {
                result.push_str("\\\\");
                result.push(ch);
            }
            '%' => result.push_str("%%"),
            '\n' | '\r' => (),
            _ => result.push(ch),
        }
    }
    result.push('"');
    result
}

pub fn rewrite(content: &str, backend: &Path, state: &Path, profile: &str) -> Option<String> {
    let prefix = format!(
        "{} --state-dir {} --profile {} run -- ",
        exec_quote(&backend.to_string_lossy()),
        exec_quote(&state.to_string_lossy()),
        exec_quote(profile)
    );
    let mut lines = vec![];
    let mut in_desktop = false;
    let mut desktop_seen = false;
    let mut main_exec = false;
    let mut dbus_written = false;
    let mut enabled = true;
    for line in content.lines() {
        if line.starts_with('[') {
            if in_desktop && !dbus_written {
                lines.push("DBusActivatable=false".into());
            }
            in_desktop = line == "[Desktop Entry]";
            desktop_seen |= in_desktop;
            lines.push(line.into());
        } else if let Some(command) = line.strip_prefix("Exec=") {
            if command.trim().is_empty() {
                return None;
            }
            main_exec |= in_desktop;
            lines.push(format!("Exec={prefix}{command}"));
        } else if line.starts_with("TryExec=") || line.starts_with("Path=") {
            // These host paths would prevent an otherwise valid launcher from opening.
        } else if in_desktop && line.starts_with("DBusActivatable=") {
            lines.push("DBusActivatable=false".into());
            dbus_written = true;
        } else {
            if in_desktop
                && (line == "Hidden=true"
                    || line == "NoDisplay=true"
                    || line == "Type=Link"
                    || line == "Type=Directory")
            {
                enabled = false;
            }
            lines.push(line.into());
        }
    }
    if in_desktop && !dbus_written {
        lines.push("DBusActivatable=false".into());
    }
    if !desktop_seen || !main_exec || !enabled {
        return None;
    }
    Some(format!("{}\n", lines.join("\n")))
}

pub fn export(options: &Options, profile: &Profile) -> Result<()> {
    let current = profile.current()?;
    let source = current.join("root/usr/share/applications");
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?).join(".local/share"),
    };
    let target = data.join("applications");
    // Profile names exclude dots: this delimiter prevents profile "a" from
    // deleting launchers belonging to profile "a-b" during stale-file cleanup.
    let prefix = format!("dnf-binfmt-{}.", options.profile);
    let backend = std::env::current_exe()?;
    let mut wanted = HashSet::new();
    if !options.dry_run {
        fs::create_dir_all(&target)?;
    }
    if source.exists() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            // Ignore symlinks and nonstandard filenames from packages.
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".desktop")
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            {
                continue;
            }
            let content = fs::read_to_string(entry.path())?;
            if let Some(output) = rewrite(&content, &backend, &options.state_dir, &options.profile)
            {
                let name = format!("{prefix}{name}");
                println!("Export {}", target.join(&name).display());
                if !options.dry_run {
                    // Rename a freshly created regular file instead of following an old symlink.
                    let temp = target.join(format!(".{name}-{}", std::process::id()));
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&temp)?;
                    std::io::Write::write_all(&mut file, output.as_bytes())?;
                    file.sync_all()?;
                    fs::rename(temp, target.join(&name))?;
                }
                wanted.insert(name);
            }
        }
    }
    if target.exists() {
        for entry in fs::read_dir(&target)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".desktop") && !wanted.contains(&name) {
                println!("Remove stale launcher {}", entry.path().display());
                if !options.dry_run {
                    fs::remove_file(entry.path())?;
                }
            }
        }
    }
    println!("Menu entries refreshed. Some package icons and working directories need manual integration in this prototype.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchers_preserve_file_codes_and_disable_host_activation() {
        let input = "[Desktop Entry]\nType=Application\nName=Example\nExec=/usr/bin/app %U\nTryExec=/usr/bin/app\nDBusActivatable=true\nActions=New;\n\n[Desktop Action New]\nName=New\nExec=/usr/bin/app --new %f\n";
        let output = rewrite(
            input,
            Path::new("/usr/libexec/dnf-binfmt"),
            Path::new("/var/lib/dnf-binfmt"),
            "default",
        )
        .unwrap();
        assert!(output.contains("run -- /usr/bin/app %U"));
        assert!(output.contains("run -- /usr/bin/app --new %f"));
        assert!(!output.contains("TryExec="));
        assert!(!output.contains("DBusActivatable=true"));
        assert_eq!(output.matches("DBusActivatable=false").count(), 1);
    }

    #[test]
    fn hidden_and_dbus_only_apps_are_not_exported() {
        for content in [
            "[Desktop Entry]\nHidden=true\nExec=app\n",
            "[Desktop Entry]\nDBusActivatable=true\n",
            "[Desktop Entry]\nExec=\n",
        ] {
            assert!(rewrite(content, Path::new("/backend"), Path::new("/state"), "test").is_none());
        }
    }

    #[test]
    fn quote_escapes_desktop_field_codes_and_shell_metacharacters() {
        assert_eq!(
            exec_quote("/path with spaces/%f"),
            "\"/path with spaces/%%f\""
        );
        assert_eq!(exec_quote("$`\""), "\"\\\\$\\\\`\\\\\"\"");
    }
}
