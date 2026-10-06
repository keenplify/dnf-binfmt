use crate::{
    environment::{CommandSpec, Profile},
    Options, Result,
};
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
        "{} run --state-dir {} --profile {} -- ",
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

pub fn graphical_launcher(root: &Path, executable: &str) -> Result<bool> {
    graphical_launchers(root, Some(executable))
}

pub fn has_graphical_launcher(root: &Path) -> Result<bool> {
    graphical_launchers(root, None)
}

fn graphical_launchers(root: &Path, executable: Option<&str>) -> Result<bool> {
    let directory = root.join("usr/share/applications");
    if !directory.is_dir() {
        return Ok(false);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|ext| ext != "desktop") {
            continue;
        }
        let content = fs::read_to_string(entry.path())?;
        let section = content.split("[Desktop Entry]").nth(1)
            .map(|text| text.split('[').next().unwrap_or(text)).unwrap_or("");
        if section.lines().any(|line| line == "Terminal=true") {
            continue;
        }
        for line in section.lines() {
            if let Some(exec) = line.strip_prefix("Exec=") {
                let first = exec.split_whitespace().next().unwrap_or("").trim_matches('"');
                if !first.is_empty() && executable.is_none_or(|executable| {
                    first == executable || (!executable.contains('/')
                        && Path::new(first).file_name().is_some_and(|name| name == executable))
                }) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// Resolve the sudo caller through the account database; never write user files
// as root or use root's HOME/XDG_DATA_HOME for automatic exports.
fn caller_export_command(
    options: &Options,
    uid: u32,
    passwd: &str,
    backend: &Path,
) -> Result<CommandSpec> {
    if uid == 0 {
        return Err("no non-root sudo caller".into());
    }
    let fields: Vec<_> = passwd.trim_end().split(':').collect();
    if fields.len() != 7
        || fields[2].parse::<u32>()? != uid
        || fields[0].is_empty()
        || fields[0].starts_with('-')
        || !Path::new(fields[5]).is_absolute()
        || fields[5] == "/"
    {
        return Err("invalid sudo caller account".into());
    }
    let mut command = CommandSpec::new("/usr/bin/runuser", vec![
        "--user".into(), fields[0].into(), "--".into(),
        backend.to_string_lossy().into_owned(), "export".into(),
        "--state-dir".into(), options.state_dir.to_string_lossy().into_owned(),
        "--profile".into(), options.profile.clone(),
    ]);
    command.env = vec![
        ("HOME".into(), fields[5].into()),
        ("USER".into(), fields[0].into()),
        ("LOGNAME".into(), fields[0].into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    Ok(command)
}

pub fn export_after_transaction(options: &Options) -> Result<()> {
    let uid: u32 = std::env::var("SUDO_UID")
        .map_err(|_| "no sudo caller; run dnf binfmt export as your desktop user")?
        .parse()?;
    if uid == 0 {
        return Err("no non-root sudo caller; run dnf binfmt export as your desktop user".into());
    }
    let account = std::process::Command::new("/usr/bin/getent")
        .args(["passwd", &uid.to_string()])
        .output()?;
    if !account.status.success() {
        return Err("could not resolve the sudo caller account".into());
    }
    let command = caller_export_command(
        options,
        uid,
        &String::from_utf8(account.stdout)?,
        &std::env::current_exe()?,
    )?;
    eprintln!("+ {}", command.display());
    let status = command.process()
        .env_clear()
        .envs(command.env.iter().cloned())
        .status()?;
    if !status.success() {
        return Err(format!("shortcut export exited with {status}").into());
    }
    Ok(())
}

// Copy only referenced package icons; keep host themes and other profiles untouched.
fn package_icon(root: &Path, value: &str) -> Option<PathBuf> {
    let root = fs::canonicalize(root).ok()?;
    let supported = |path: &Path| path.extension().is_some_and(|ext| matches!(ext.to_str(), Some("png" | "svg" | "xpm")));
    let confined = |path: PathBuf| fs::canonicalize(path).ok()
        .filter(|path| path.starts_with(&root) && path.is_file() && supported(path));
    if value.starts_with('/') {
        return confined(root.join(value.trim_start_matches('/')));
    }
    if value.is_empty() || value.contains('/') || value.contains('\\') {
        return None;
    }
    fn collect(directory: &Path, value: &str, depth: usize, matches: &mut Vec<PathBuf>) {
        if depth > 8 { return; }
        let Ok(entries) = fs::read_dir(directory) else { return; };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                collect(&path, value, depth + 1, matches);
            } else if path.file_name().is_some_and(|name| name == value)
                || path.file_stem().is_some_and(|name| name == value) {
                matches.push(path);
            }
        }
    }
    let mut matches = vec![];
    collect(&root.join("usr/share/icons"), value, 0, &mut matches);
    collect(&root.join("usr/share/pixmaps"), value, 0, &mut matches);
    // Prefer scalable icons, then the largest raster size, with stable tie breaking.
    matches.sort_by_key(|path| {
        let size = path.components().filter_map(|component| component.as_os_str().to_str())
            .filter_map(|component| component.split_once('x'))
            .filter_map(|(width, _)| width.parse::<u32>().ok()).max().unwrap_or(0);
        (std::cmp::Reverse(path.extension().is_some_and(|ext| ext == "svg")), std::cmp::Reverse(size), path.clone())
    });
    matches.into_iter().find_map(confined)
}

fn export_icons(content: &str, root: &Path, directory: &Path, launcher: &str,
    dry_run: bool, wanted: &mut HashSet<String>) -> Result<String> {
    let mut lines = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if let Some(value) = line.strip_prefix("Icon=") {
            if let Some(source) = package_icon(root, value) {
                let extension = source.extension().unwrap().to_string_lossy();
                let name = format!("{launcher}-{index}.{extension}");
                let destination = directory.join(&name);
                if !dry_run {
                    fs::create_dir_all(directory)?;
                    let temp = directory.join(format!(".{name}-{}", std::process::id()));
                    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
                    std::io::Write::write_all(&mut file, &fs::read(source)?)?;
                    file.sync_all()?;
                    fs::rename(temp, &destination)?;
                }
                wanted.insert(name);
                let path = destination.to_string_lossy().replace('\\', "\\\\");
                lines.push(format!("Icon={path}"));
                continue;
            }
        }
        lines.push(line.to_owned());
    }
    Ok(format!("{}\n", lines.join("\n")))
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
    let icons = data.join("dnf-binfmt").join(&options.profile).join("icons");
    let mut wanted_icons = HashSet::new();
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
                let output = export_icons(&output, &current.join("root"), &icons, &name, options.dry_run, &mut wanted_icons)?;
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
    if icons.is_dir() {
        for entry in fs::read_dir(&icons)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && !wanted_icons.contains(&name) && !options.dry_run {
                fs::remove_file(entry.path())?;
            }
        }
    }
    println!("Menu entries and package icons refreshed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_icons_are_copied_and_guest_paths_cannot_escape() {
        let directory = std::env::temp_dir().join(format!("dnf-binfmt-icons-{}", std::process::id()));
        let root = directory.join("root");
        let small = root.join("usr/share/icons/hicolor/16x16/apps");
        let large = root.join("usr/share/icons/hicolor/256x256/apps");
        fs::create_dir_all(&small).unwrap();
        fs::create_dir_all(&large).unwrap();
        fs::write(small.join("discord.png"), b"small").unwrap();
        fs::write(large.join("discord.png"), b"large").unwrap();
        fs::write(directory.join("outside.png"), b"outside").unwrap();
        std::os::unix::fs::symlink(directory.join("outside.png"), large.join("escape.png")).unwrap();
        assert_eq!(package_icon(&root, "discord"), Some(large.join("discord.png")));
        assert!(package_icon(&root, "escape").is_none());
        assert!(package_icon(&root, "/../outside.png").is_none());
        assert!(package_icon(&root, "../outside").is_none());
        let icons = directory.join("exported");
        let mut wanted = HashSet::new();
        let output = export_icons("[Desktop Entry]\nIcon=discord\n[Desktop Action New]\nIcon=/usr/share/icons/hicolor/16x16/apps/discord.png\n", &root, &icons, "dnf-binfmt-default.discord.desktop", false, &mut wanted).unwrap();
        assert!(output.contains(&format!("Icon={}/dnf-binfmt-default.discord.desktop-1.png", icons.display())));
        assert_eq!(fs::read(icons.join("dnf-binfmt-default.discord.desktop-1.png")).unwrap(), b"large");
        assert_eq!(fs::read(icons.join("dnf-binfmt-default.discord.desktop-3.png")).unwrap(), b"small");
        assert_eq!(wanted.len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn desktop_entries_identify_gui_shell_launchers_without_matching_other_apps() {
        let root = std::env::temp_dir().join(format!("dnf-binfmt-gui-desktop-{}", std::process::id()));
        let source = root.join("usr/share/applications");
        fs::create_dir_all(&source).unwrap();
        let file = source.join("bootstrap.desktop");
        fs::write(&file, "[Desktop Entry]\nType=Application\nExec=/usr/bin/bootstrap %U\n").unwrap();
        assert!(graphical_launcher(&root, "/usr/bin/bootstrap").unwrap());
        assert!(graphical_launcher(&root, "bootstrap").unwrap());
        assert!(!graphical_launcher(&root, "/usr/bin/other").unwrap());
        fs::write(file, "[Desktop Entry]\nTerminal=true\nExec=/usr/bin/bootstrap\n").unwrap();
        assert!(!graphical_launcher(&root, "/usr/bin/bootstrap").unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automatic_export_uses_sudo_account_and_current_profile() {
        let options = Options::parse(vec!["install".into(), "--profile".into(), "tools".into(), "app".into()]).unwrap();
        let command = caller_export_command(&options, 1000, "alice:x:1000:1000:Alice:/home/alice:/bin/bash\n", Path::new("/usr/libexec/dnf-binfmt")).unwrap();
        assert_eq!(command.program, "/usr/bin/runuser");
        assert_eq!(command.args, ["--user", "alice", "--", "/usr/libexec/dnf-binfmt", "export", "--state-dir", "/var/lib/dnf-binfmt", "--profile", "tools"]);
        assert!(command.env.contains(&("HOME".into(), "/home/alice".into())));
        assert!(!command.env.iter().any(|(name, _)| name == "XDG_DATA_HOME"));
        for (uid, account) in [(0, "root:x:0:0:root:/root:/bin/bash"), (1000, "bob:x:1001:1001:Bob:/home/bob:/bin/bash"), (1000, "alice:x:1000:1000:Alice:/:/bin/bash")] {
            assert!(caller_export_command(&options, uid, account, Path::new("/backend")).is_err());
        }
    }

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
        assert!(output.contains("--profile \"default\" -- /usr/bin/app %U"));
        assert!(output.contains("--profile \"default\" -- /usr/bin/app --new %f"));
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
