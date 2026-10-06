use crate::{
    environment::{launch_command, CommandSpec, Profile},
    inspection::Inspection,
    Options, Result,
};
use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::IsTerminal,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
};

unsafe extern "C" {
    fn geteuid() -> u32;
}

fn vm_directory(base: &Path, profile: &Profile, image: &Path, software: bool) -> PathBuf {
    let mut hash = DefaultHasher::new();
    image.hash(&mut hash);
    software.hash(&mut hash);
    profile.overlays.hash(&mut hash);
    // An overlay replaced in place must not reuse the previous VM either.
    for overlay in &profile.overlays {
        if let Ok(metadata) = fs::metadata(overlay) {
            metadata.len().hash(&mut hash);
            metadata.modified().ok().hash(&mut hash);
        }
    }
    base.join("dnf-binfmt").join(format!("{:016x}", hash.finish()))
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "runtime directory must be a private directory owned by you: {}",
            path.display()
        ).into());
    }
    Ok(())
}

pub fn prepare(command: &CommandSpec) -> Result<()> {
    let (_, directory) = command.env.iter()
        .find(|(key, _)| key == "XDG_RUNTIME_DIR")
        .ok_or("missing managed VM runtime directory")?;
    let directory = Path::new(directory);
    private_directory(directory.parent().ok_or("runtime directory has no parent")?)?;
    private_directory(directory)
}

fn video_bridge_running(proc_root: &Path) -> bool {
    let Ok(processes) = fs::read_dir(proc_root) else { return false; };
    processes.flatten().any(|entry| {
        entry.file_name().to_string_lossy().bytes().all(|c| c.is_ascii_digit())
            && entry.metadata().is_ok_and(|meta| meta.uid() == unsafe { geteuid() })
            && fs::read_link(entry.path().join("exe")).is_ok_and(|exe| {
                exe.file_name().is_some_and(|name| name == "xwaylandvideobridge")
            })
    })
}

pub fn prepare_video_bridge(options: &Options, capabilities: &Inspection) -> Result<()> {
    let gui = capabilities.gui() || options.graphics != "auto";
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !gui || !wayland {
        return Ok(());
    }
    if std::env::var_os("DISPLAY").is_none_or(|value| value.is_empty()) {
        eprintln!("XWayland Video Bridge needs the host XWayland DISPLAY; launch from your desktop session.");
        return Ok(());
    }
    let executable = Path::new("/usr/bin/xwaylandvideobridge");
    if !executable.is_file() {
        eprintln!("For Wayland screen sharing, install xwaylandvideobridge on the host: sudo dnf install xwaylandvideobridge");
        return Ok(());
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { geteuid() })));
    if !base.is_absolute() {
        return Err("XDG_RUNTIME_DIR must be an absolute path".into());
    }
    let directory = base.join("dnf-binfmt");
    private_directory(&directory)?;
    let lock = fs::OpenOptions::new().write(true).create(true).truncate(false)
        .mode(0o600).open(directory.join("video-bridge.lock"))?;
    // Other launches need not wait for this optional host helper.
    if lock.try_lock().is_err() || video_bridge_running(Path::new("/proc")) {
        return Ok(());
    }
    let log = directory.join(format!("video-bridge-{}.log", std::process::id()));
    let output = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&log)?;
    // Inherit the original host Wayland/session environment, before muvm's
    // XDG_RUNTIME_DIR is applied. Detach stdio so a tee pipeline can finish.
    std::process::Command::new(executable)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(output)
        .spawn()?;
    eprintln!("Started host XWayland Video Bridge for screen sharing (log: {}).", log.display());
    Ok(())
}

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

fn compatibility_assets() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let installed = executable.parent().ok_or("backend has no parent")?
        .join("dnf-binfmt-runtime");
    let development = executable.ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "target"))
        .and_then(Path::parent)
        .map(|root| root.join("build/runtime"));
    for directory in std::iter::once(installed).chain(development) {
        if directory.join("native/dnf-binfmt-mmap.so").is_file()
            && directory.join("rootfs.erofs").is_file() {
            return Ok(directory);
        }
    }
    Err("FEX compatibility helper is missing; run make and sudo make install".into())
}

fn application_arguments(arguments: &[String]) -> Vec<String> {
    let mut result = arguments.to_vec();
    if arguments.first().is_some_and(|argument| matches!(argument.as_str(), "discord" | "/usr/bin/discord")) {
        // Chromium's 15-second child IPC deadline can expire while FEX is
        // still initializing Discord. Keep its network service alive through
        // the slow startup; preserve any explicit caller override.
        let switches = arguments.iter().skip(1).take_while(|argument| argument.as_str() != "--");
        if !switches.clone().any(|argument| argument == "--ipc-connection-timeout"
            || argument.starts_with("--ipc-connection-timeout=")) {
            result.insert(1, "--ipc-connection-timeout=180".into());
        }
    }
    result
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
    let assets = if capabilities.gui() || software {
        Some(compatibility_assets()?)
    } else {
        None
    };
    let runtime_profile = Profile {
        directory: profile.directory.clone(),
        releasever: profile.releasever.clone(),
        overlays: profile.overlays.iter().cloned()
            .chain(assets.iter().map(|directory| directory.join("rootfs.erofs")))
            .collect(),
    };
    let mut command = launch_command(&runtime_profile, image, &application_arguments(&options.args));
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        command.args.insert(0, "-t".into());
    }
    if software {
        command.args.insert(0, "--gpu-mode=software".into());
    }
    // muvm checks its runtime lock before processing -f. A shared runtime
    // directory can silently send this command to a VM with a different image.
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { geteuid() })));
    if !base.is_absolute() {
        return Err("XDG_RUNTIME_DIR must be an absolute path".into());
    }
    command.env.push((
        "XDG_RUNTIME_DIR".into(),
        vm_directory(&base, &runtime_profile, image, software).to_string_lossy().into_owned(),
    ));
    // muvm uses the host runtime directory to discover the audio socket.
    // Preserve that discovery when moving its lock and server sockets.
    if std::env::var_os("PULSE_SERVER").is_none() && base.join("pulse/native").exists() {
        command.env.push((
            "PULSE_SERVER".into(),
            format!("unix:{}", base.join("pulse/native").display()),
        ));
    }
    let mut environment = vec![];
    if capabilities.gui() || software {
        environment.extend(["GDK_BACKEND=x11", "NO_AT_BRIDGE=1", "GTK_MODULES="]);
    }
    if software {
        environment.extend([
            "LIBGL_ALWAYS_SOFTWARE=1",
            "GALLIUM_DRIVER=llvmpipe",
            "MESA_LOADER_DRIVER_OVERRIDE=swrast",
            "LIBGL_DRI3_DISABLE=1",
        ]);
    }
    let args = environment
        .into_iter()
        .flat_map(|value| ["-e".to_string(), value.to_string()])
        .collect::<Vec<_>>();
    command.args.splice(0..0, args);
    if let Some(directory) = assets {
        // Both ELF loaders resolve the same basename: native ARM code from
        // this directory, x86 code from the compatibility overlay's /usr/lib64.
        // This also covers FEX re-execution for application subprocesses.
        let mut libraries = directory.join("native").to_string_lossy().into_owned();
        if let Ok(existing) = std::env::var("LD_LIBRARY_PATH") {
            if !existing.is_empty() {
                libraries.push(':');
                libraries.push_str(&existing);
            }
        }
        command.args.splice(0..0, [
            "-e".into(), "LD_PRELOAD=dnf-binfmt-mmap.so".into(),
            "-e".into(), format!("LD_LIBRARY_PATH={libraries}"),
        ]);
    }
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
    let mut bridge = CommandSpec::new("/usr/bin/python3", args);
    bridge.env = command.env;
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_bridge_reuses_only_the_expected_host_process() {
        let root = std::env::temp_dir().join(format!("dnf-binfmt-video-proc-{}", std::process::id()));
        fs::create_dir_all(root.join("123")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/other", root.join("123/exe")).unwrap();
        assert!(!video_bridge_running(&root));
        fs::remove_file(root.join("123/exe")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/xwaylandvideobridge", root.join("123/exe")).unwrap();
        assert!(video_bridge_running(&root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vm_reuse_is_scoped_to_generation_and_overlay_order() {
        let mut profile = Profile {
            directory: "/var/lib/dnf-binfmt/default".into(),
            releasever: "44".into(),
            overlays: vec![],
        };
        let base = Path::new("/run/user/1000");
        let image = Path::new("/var/lib/dnf-binfmt/default/generations/one/rootfs.erofs");
        let first = vm_directory(base, &profile, image, false);
        assert_ne!(first, base.to_path_buf());
        assert_ne!(first, vm_directory(base, &profile, image, true));
        assert_eq!(first, vm_directory(base, &profile, image, false));
        assert_ne!(first, vm_directory(base, &profile, Path::new("/var/lib/dnf-binfmt/default/generations/two/rootfs.erofs"), false));
        profile.overlays = vec!["/one.erofs".into(), "/two.erofs".into()];
        let overlays = vm_directory(base, &profile, image, false);
        assert_ne!(first, overlays);
        profile.overlays.reverse();
        assert_ne!(overlays, vm_directory(base, &profile, image, false));
    }

    #[test]
    fn runtime_directory_rejects_symlinks_and_public_permissions() {
        let root = std::env::temp_dir().join(format!("dnf-binfmt-vm-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let directory = root.join("vm");
        private_directory(&directory).unwrap();
        private_directory(&directory).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        assert!(private_directory(&link).is_err());
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&directory).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn discord_ipc_deadline_preserves_overrides_and_url_arguments() {
        let args = ["/usr/bin/discord", "--url", "--", "discord://channel"]
            .map(str::to_owned).to_vec();
        assert_eq!(application_arguments(&args), ["/usr/bin/discord", "--ipc-connection-timeout=180", "--url", "--", "discord://channel"]);
        for args in [vec!["discord", "--ipc-connection-timeout=90"],
            vec!["discord", "--ipc-connection-timeout", "90"], vec!["/usr/bin/other", "--url"]] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(application_arguments(&args), args);
        }
    }

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
        assert!(command.args.contains(&"--gpu-mode=software".into()));
        assert!(command.args.contains(&"LD_PRELOAD=dnf-binfmt-mmap.so".into()));
        assert!(command.args.iter().any(|arg| arg.ends_with("build/runtime/rootfs.erofs")));
        let software_runtime = command.env.clone();
        assert!(command.args.contains(&"LIBGL_DRI3_DISABLE=1".into()));
        assert!(command.args.contains(&"MESA_LOADER_DRIVER_OVERRIDE=swrast".into()));
        assert!(command.env.iter().any(|(key, value)| key == "XDG_RUNTIME_DIR" && value.contains("/dnf-binfmt/")));
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
        assert!(command.env.iter().any(|(key, value)| key == "XDG_RUNTIME_DIR" && value.contains("/dnf-binfmt/")));
        assert!(!command.args.contains(&"LIBGL_ALWAYS_SOFTWARE=1".into()));
        assert!(!command.args.contains(&"--gpu-mode=software".into()));
        assert_ne!(software_runtime, command.env);
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
