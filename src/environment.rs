use crate::{
    cli::{safe_absolute, Options},
    desktop, inspection, native, runtime, Result,
};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::{
    fs::{symlink, MetadataExt, PermissionsExt},
    process::CommandExt,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            env: vec![],
        }
    }
    pub fn display(&self) -> String {
        let command = std::iter::once(&self.program)
            .chain(&self.args)
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        if self.env.is_empty() {
            command
        } else {
            let environment = self.env.iter()
                .map(|(key, value)| shell_quote(&format!("{key}={value}")))
                .collect::<Vec<_>>()
                .join(" ");
            format!("env {environment} {command}")
        }
    }
    pub fn process(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(self.env.iter().cloned());
        command
    }
    pub fn checked(&self) -> Result<()> {
        eprintln!("+ {}", self.display());
        let status = self.process().status()?;
        if !status.success() {
            return Err(format!("{} exited with {status}", self.program).into());
        }
        Ok(())
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub directory: PathBuf,
    pub releasever: String,
    pub overlays: Vec<PathBuf>,
}

impl Profile {
    pub fn load(options: &Options) -> Result<Self> {
        let directory = options.state_dir.join(&options.profile);
        let content = fs::read_to_string(directory.join("profile.conf")).map_err(|e| {
            format!(
                "profile is not initialized ({e}); run sudo dnf binfmt init --releasever NUMBER"
            )
        })?;
        let mut releasever = None;
        let mut overlays = vec![];
        for line in content.lines() {
            if let Some(value) = line.strip_prefix("releasever=") {
                if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
                    return Err("invalid stored releasever".into());
                }
                releasever = Some(value.into());
            } else if let Some(value) = line.strip_prefix("overlay=") {
                let path = PathBuf::from(value);
                if !safe_absolute(&path) {
                    return Err("invalid stored overlay path".into());
                }
                overlays.push(path);
            } else {
                return Err("unknown profile setting".into());
            }
        }
        Ok(Self {
            directory,
            releasever: releasever.ok_or("profile lacks releasever")?,
            overlays,
        })
    }
    pub fn current(&self) -> Result<PathBuf> {
        let current = fs::canonicalize(self.directory.join("current"))?;
        let generations = fs::canonicalize(self.directory.join("generations"))?;
        if !current.starts_with(&generations) || current.parent() != Some(generations.as_path()) {
            return Err("current generation points outside the profile".into());
        }
        Ok(current)
    }
}

pub fn dnf_command(
    profile: &Profile,
    root: &Path,
    action: &str,
    packages: &[String],
) -> CommandSpec {
    let mut args = vec![
        format!("--config={}", profile.directory.join("dnf.conf").display()),
        format!("--installroot={}", root.display()),
        "--forcearch=x86_64".into(),
        format!("--releasever={}", profile.releasever),
        "--disable-plugin=*".into(),
        format!(
            "--setopt=reposdir={}",
            profile.directory.join("repos.d").display()
        ),
        format!(
            "--setopt=logdir={}",
            root.join("var/log/dnf-binfmt").display()
        ),
        format!(
            "--setopt=cachedir={}",
            root.join("var/cache/dnf-binfmt").display()
        ),
        format!(
            "--setopt=system_cachedir={}",
            root.join("var/cache/dnf-binfmt").display()
        ),
        "--setopt=tsflags=noscripts,notriggers,noplugins".into(),
        "--setopt=install_weak_deps=False".into(),
        "--setopt=protected_packages=bash,glibc,filesystem".into(),
        "--setopt=protect_running_kernel=False".into(),
        action.into(),
        "--".into(),
    ];
    args.extend_from_slice(packages);
    CommandSpec::new("/usr/bin/dnf5", args)
}

pub fn image_command(root: &Path, image: &Path) -> CommandSpec {
    CommandSpec::new(
        "/usr/bin/mkfs.erofs",
        vec![
            "-b4096".into(),
            "-zlz4".into(),
            "--all-root".into(),
            "--exclude-path=var/cache".into(),
            "--exclude-path=var/log".into(),
            string(image),
            string(root),
        ],
    )
}

pub fn launch_command(profile: &Profile, image: &Path, args: &[String]) -> CommandSpec {
    let mut result = vec![
        "--emu=fex".into(),
        "-i".into(),
        "-e".into(),
        "FEX_ROOTFS=/run/fex-emu/rootfs".into(),
        "-f".into(),
        string(image),
    ];
    for overlay in &profile.overlays {
        result.extend(["-f".into(), string(overlay)]);
    }
    result.extend([
        "--".into(),
        "/usr/bin/FEXBash".into(),
        "-c".into(),
        "exec \"$@\"".into(),
        "dnf-binfmt".into(),
    ]);
    result.extend_from_slice(args);
    CommandSpec::new("/usr/bin/muvm", result)
}

fn root_user() -> bool {
    unsafe { geteuid() == 0 }
}
fn require_root() -> Result<()> {
    if !root_user() {
        return Err(
            "this operation requires sudo; application launch/export must run as your normal user"
                .into(),
        );
    }
    Ok(())
}

// Root never follows user-controlled state paths. Existing ancestors must be
// root-owned and not writable by a group or other users, including custom paths.
fn trusted_path(path: &Path) -> Result<()> {
    if !safe_absolute(path) {
        return Err("unsafe state path".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta)
                if meta.file_type().is_symlink()
                    || !meta.is_dir()
                    || meta.uid() != 0
                    || meta.mode() & 0o022 != 0 =>
            {
                return Err(format!(
                    "state ancestor must be a root-owned, non-writable directory: {}",
                    ancestor.display()
                )
                .into());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(format!("untrusted configuration file: {}", path.display()).into());
    }
    Ok(())
}

struct ProfileLock(File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        // Explicitly release the lock even if a concurrent fork briefly retains
        // an inherited descriptor before exec closes it.
        let _ = self.0.unlock();
    }
}

fn lock(directory: &Path) -> Result<ProfileLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".lock"))?;
    file.try_lock()
        .map_err(|e| format!("profile is busy: {e}"))?;
    Ok(ProfileLock(file))
}

fn write_new(path: &Path, value: &str) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn init(options: &Options) -> Result<u8> {
    let release = options
        .releasever
        .as_ref()
        .ok_or("init requires --releasever NUMBER")?;
    let profile = options.state_dir.join(&options.profile);
    let key = PathBuf::from(format!(
        "/etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-{release}-primary"
    ));
    if options.dry_run {
        println!(
            "Create profile {} for Fedora {release}, using Fedora x86_64 repositories and key {}",
            profile.display(),
            key.display()
        );
        return Ok(0);
    }
    require_root()?;
    trusted_path(&profile)?;
    if profile.exists() {
        return Err("profile already exists; refusing to overwrite it".into());
    }
    if !key.is_file() {
        return Err(format!("missing Fedora signing key {}", key.display()).into());
    }
    for overlay in &options.overlays {
        if !overlay.is_file() {
            return Err(format!("missing overlay {}", overlay.display()).into());
        }
    }
    fs::create_dir_all(&options.state_dir)?;
    let _lock = lock(&options.state_dir)?;
    fs::create_dir(&profile)?;
    let result: Result<()> = (|| {
        fs::create_dir(profile.join("generations"))?;
        fs::create_dir(profile.join("repos.d"))?;
        let mut config = format!("releasever={release}\n");
        for overlay in &options.overlays {
            config.push_str(&format!("overlay={}\n", overlay.display()));
        }
        write_new(&profile.join("profile.conf"), &config)?;
        // Copy the key: installroot handling must not depend on host symlinks.
        write_new(&profile.join("fedora-key.asc"), &fs::read_to_string(key)?)?;
        write_new(
            &profile.join("dnf.conf"),
            "[main]\ngpgcheck=1\nlocalpkg_gpgcheck=1\nplugins=0\n",
        )?;
        let repos = format!("[fedora]\nname=Fedora {release}\nmetalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-{release}&arch=$basearch\nenabled=1\ngpgcheck=1\ngpgkey=file://{0}/fedora-key.asc\n\n[updates]\nname=Fedora {release} updates\nmetalink=https://mirrors.fedoraproject.org/metalink?repo=updates-released-f{release}&arch=$basearch\nenabled=1\ngpgcheck=1\ngpgkey=file://{0}/fedora-key.asc\n", profile.display());
        write_new(&profile.join("repos.d/fedora.repo"), &repos)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&profile);
    }
    result?;
    println!(
        "Initialized {}. Next: sudo dnf binfmt install PACKAGE",
        options.profile
    );
    Ok(0)
}

fn package_args(options: &Options) -> Result<Vec<String>> {
    let mut args = vec![];
    for arg in &options.args {
        if options.command == "install" && arg.ends_with(".rpm") {
            if arg.contains("://") {
                return Err(
                    "download remote RPMs first; URL installation is not supported yet".into(),
                );
            }
            let path = fs::canonicalize(arg)?;
            let query = Command::new("/usr/bin/rpm")
                .args(["-qp", "--queryformat", "%{ARCH}", "--"])
                .arg(&path)
                .output()?;
            let arch = String::from_utf8(query.stdout)?;
            if !query.status.success() || !matches!(arch.as_str(), "x86_64" | "noarch") {
                return Err(format!("{} is not an x86_64/noarch RPM", path.display()).into());
            }
            args.push(string(&path));
        } else {
            args.push(arg.clone());
        }
    }
    Ok(args)
}

fn mutate(options: &Options, profile: &Profile) -> Result<u8> {
    let packages = package_args(options)?;
    let previous = if profile.directory.join("current").exists() {
        Some(profile.current()?)
    } else {
        None
    };
    if previous.is_none() && options.command != "install" {
        return Err("profile has no installed generation".into());
    }
    let mut requested = packages;
    if options.command == "install" {
        for package in &requested.clone() {
            if package.ends_with(".rpm") {
                let libraries = inspection::rpm(Path::new(package))?.missing();
                eprintln!(
                    "ELF inspection: adding {} system-library requirements from {}",
                    libraries.len(),
                    package
                );
                requested.extend(libraries);
            }
        }
        requested.sort();
        requested.dedup();
    }
    if previous.is_none() {
        requested.extend([
            "bash.x86_64".into(),
            "glibc.x86_64".into(),
            "filesystem.x86_64".into(),
        ]);
    }
    let planned = profile.directory.join("generations/next");
    if options.dry_run {
        if options.command == "upgrade" {
            if let Some(previous) = &previous {
                println!(
                    "Native check: {}",
                    native::query(profile, &previous.join("root"), std::env::consts::ARCH)
                        .display()
                );
            }
        }
        println!("Stage a new generation; keep current intact until both DNF and image creation succeed.");
        println!(
            "{}",
            dnf_command(profile, &planned.join("root"), &options.command, &requested).display()
        );
        println!(
            "{}",
            image_command(&planned.join("root"), &planned.join("rootfs.erofs")).display()
        );
        println!("Publish current symlink atomically. RPM scripts/triggers remain skipped.");
        return Ok(0);
    }
    require_root()?;
    trusted_path(&profile.directory)?;
    private_file(&profile.directory.join("profile.conf"))?;
    private_file(&profile.directory.join("dnf.conf"))?;
    trusted_path(&profile.directory.join("repos.d"))?;
    for entry in fs::read_dir(profile.directory.join("repos.d"))? {
        private_file(&entry?.path())?;
    }
    let _lock = lock(&profile.directory)?;
    if options.command == "upgrade" {
        native::check(profile, &profile.current()?.join("root"))?;
    }
    let id = stage_generation(profile, &options.command, &requested, |command| {
        command.checked()
    })?;
    println!("Published generation {id}.");
    // Package publication succeeded. An export failure must not report the
    // installation as failed or roll back the published generation.
    if let Err(error) = desktop::export_after_transaction(options) {
        eprintln!("Packages updated, but menu entries were not refreshed: {error}. Run dnf binfmt export as your normal user.");
    }
    Ok(0)
}

fn compile_settings_schemas(root: &Path) -> Result<()> {
    let directory = root.join("usr/share/glib-2.0/schemas");
    if !directory.exists() {
        return Ok(());
    }
    // The output belongs to the staged root and must not follow a package
    // symlink into the host filesystem. GLib's cache format is portable.
    for component in ["usr", "usr/share", "usr/share/glib-2.0", "usr/share/glib-2.0/schemas"] {
        if !fs::symlink_metadata(root.join(component))?.is_dir() {
            return Err("GSettings schema directory must be a regular directory in the managed root".into());
        }
    }
    let cache = directory.join("gschemas.compiled");
    if fs::symlink_metadata(&cache).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("GSettings schema cache must be a regular file in the managed root".into());
    }
    CommandSpec::new("/usr/bin/glib-compile-schemas", vec![string(&directory)]).checked()
}

fn stage_generation(
    profile: &Profile,
    action: &str,
    requested: &[String],
    mut run: impl FnMut(CommandSpec) -> Result<()>,
) -> Result<String> {
    // Resolve current again under the lock, avoiding lost updates between writers.
    let previous = if profile.directory.join("current").exists() {
        Some(profile.current()?)
    } else {
        None
    };
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let generation = profile.directory.join("generations").join(&id);
    fs::create_dir(&generation)?;
    let mut published = false;
    let result: Result<()> = (|| {
        let root = generation.join("root");
        if let Some(previous) = previous {
            run(CommandSpec::new(
                "/usr/bin/cp",
                vec![
                    "-a".into(),
                    "--reflink=auto".into(),
                    "--".into(),
                    string(&previous.join("root")),
                    string(&root),
                ],
            ))?;
        } else {
            fs::create_dir(&root)?;
        }
        eprintln!("Experimental transaction: skipping RPM scripts and triggers.");
        run(dnf_command(profile, &root, action, requested))?;
        // DNF repository installs also get the same global ELF audit. Resolving
        // newly found providers may introduce another library, so repeat with
        // a bound. Unresolved providers abort publication, preserving current.
        for pass in 0..4 {
            let mut capabilities = inspection::tree(&root)?;
            if capabilities.gui() {
                // Chromium and other X11 clients dlopen this library; it is
                // absent from ELF DT_NEEDED and bootstrap RPM dependencies.
                capabilities.needed.insert("libX11-xcb.so.1".into());
                // Weak dependencies are disabled. Mesa loads its software
                // driver and GBM backend dynamically, so ELF auditing alone
                // misses the modules required by the default graphics mode.
                capabilities.needed.extend([
                    "libEGL_mesa.so.0".into(), "libGLX_mesa.so.0".into(),
                    // A desktop bootstrapper can download its real payload
                    // outside the RPM root. Provide the common desktop ABI
                    // there too, instead of relying on the ARM host's GTK.
                    "libgtk-3.so.0".into(), "libgbm.so.1".into(),
                    "libpulse.so.0".into(),
                    // GTK recommends these backends, but DNF weak deps are
                    // disabled here. They are runtime modules, not DT_NEEDED.
                    "libdconf.so.1".into(), "libdconfsettings.so".into(),
                    "libcanberra-gtk3.so.0".into(), "libcanberra-gtk3-module.so".into(),
                ]);
            }
            let missing = capabilities.missing();
            if missing.is_empty() {
                break;
            }
            if pass == 3 {
                return Err(format!("unresolved x86-64 libraries: {}", missing.join(", ")).into());
            }
            eprintln!("Resolving {} missing ELF library providers", missing.len());
            run(dnf_command(profile, &root, "install", &missing))?;
        }
        if inspection::tree(&root)?.gui() {
            let mut modules = Vec::new();
            if !root.join("usr/lib64/gbm/dri_gbm.so").exists() {
                modules.push("mesa-dri-drivers.x86_64".into());
            }
            // muvm selects GTK's XIM input context for X11. Fedora splits
            // that dynamically loaded module out of the main gtk3 package.
            if !root.join("usr/lib64/gtk-3.0/3.0.0/immodules/im-xim.so").exists() {
                modules.push("gtk3-immodule-xim.x86_64".into());
            }
            if !modules.is_empty() {
                run(dnf_command(profile, &root, "install", &modules))?;
            }
        }
        if !root.join("usr/bin/bash").is_file() {
            return Err("transaction left no x86-64 bash; refusing publication".into());
        }
        // RPM triggers remain disabled. Generate the portable settings cache
        // explicitly before sealing the immutable image.
        compile_settings_schemas(&root)?;
        run(image_command(&root, &generation.join("rootfs.erofs")))?;
        File::open(generation.join("rootfs.erofs"))?.sync_all()?;
        let pending = profile
            .directory
            .join(format!(".current-{}", std::process::id()));
        symlink(Path::new("generations").join(&id), &pending)?;
        fs::rename(&pending, profile.directory.join("current"))?;
        published = true;
        File::open(&profile.directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && !published {
        let _ = fs::remove_dir_all(&generation);
    }
    result?;
    Ok(id)
}

fn doctor() -> Result<u8> {
    let mut missing = false;
    for path in [
        "/usr/bin/dnf5",
        "/usr/bin/rpm",
        "/usr/bin/mkfs.erofs",
        "/usr/bin/muvm",
        "/usr/bin/FEXBash",
        "/usr/bin/binfmt-dispatcher",
        "/usr/bin/readelf",
        "/usr/bin/rpm2cpio",
        "/usr/bin/python3",
        "/usr/bin/xdg-dbus-proxy",
        "/dev/kvm",
    ] {
        let exists = Path::new(path).exists();
        println!("{} {path}", if exists { "OK     " } else { "MISSING" });
        missing |= !exists;
    }
    let arch = std::env::consts::ARCH;
    println!("Host architecture: {arch} (runtime target: aarch64)");
    missing |= arch != "aarch64";
    if let Ok(output) = Command::new("/usr/bin/getconf").arg("PAGESIZE").output() {
        println!(
            "Host page size: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    println!("Host dependencies: sudo dnf install binfmt-dispatcher fex-emu muvm erofs-utils binutils python3 xdg-dbus-proxy");
    println!("Presence checks only: KVM permissions, GPU support, and application compatibility need a real launch.");
    Ok(if missing { 1 } else { 0 })
}

pub fn execute(options: Options) -> Result<u8> {
    match options.command.as_str() {
        "doctor" => return doctor(),
        "init" => return init(&options),
        "inspect" => {
            let inspection = inspection::rpm(Path::new(&options.args[0]))?;
            println!(
                "x86-64 ELF files: {}\nGUI: {}\nSession bus: {}",
                inspection.elf_count,
                inspection.gui(),
                inspection.session_bus()
            );
            println!("External library requirements (bundled providers excluded):");
            for capability in inspection.missing() {
                println!("  {capability}");
            }
            println!("Automatic policy: software rendering for detected GUI apps; filtered session bus for detected bus clients.");
            return Ok(0);
        }
        _ => (),
    }
    let profile = Profile::load(&options)?;
    match options.command.as_str() {
        "install" | "upgrade" | "remove" => mutate(&options, &profile),
        "list" => {
            let current = profile.current()?;
            let spec = CommandSpec::new(
                "/usr/bin/rpm",
                vec![
                    "--root".into(),
                    string(&current.join("root")),
                    "-qa".into(),
                    "--queryformat".into(),
                    "%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}\\n".into(),
                ],
            );
            if options.dry_run {
                println!("{}", spec.display());
            } else {
                spec.checked()?;
            }
            Ok(0)
        }
        "run" => {
            let current = profile.current()?;
            let image = current.join("rootfs.erofs");
            if !image.is_file() {
                return Err("current image is missing".into());
            }
            for overlay in &profile.overlays {
                if !overlay.is_file() {
                    return Err(format!("missing overlay {}", overlay.display()).into());
                }
            }
            let capabilities = inspection::application(&current.join("root"), &options.args[0])?;
            let spec = runtime::plan(&profile, &image, &options, &capabilities)?;
            if options.dry_run {
                println!("{}", spec.display());
                return Ok(0);
            }
            if root_user() {
                return Err("run applications without sudo".into());
            }
            if std::env::consts::ARCH != "aarch64" {
                return Err("muvm/FEX runtime requires an aarch64 host".into());
            }
            if let Err(error) = runtime::prepare_video_bridge(&options, &capabilities) {
                eprintln!("Could not start host XWayland Video Bridge: {error}");
            }
            runtime::prepare(&spec)?;
            Err(spec.process().exec().into())
        }
        "export" => {
            if !options.dry_run && root_user() {
                return Err("export must run as your normal desktop user".into());
            }
            desktop::export(&options, &profile)?;
            Ok(0)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> Profile {
        let directory = std::env::temp_dir().join(format!(
            "dnf-binfmt-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(directory.join("generations/old/root/usr/bin")).unwrap();
        fs::write(
            directory.join("generations/old/root/usr/bin/bash"),
            "old payload",
        )
        .unwrap();
        fs::write(directory.join("generations/old/rootfs.erofs"), "old image").unwrap();
        symlink("generations/old", directory.join("current")).unwrap();
        Profile {
            directory,
            releasever: "44".into(),
            overlays: vec![],
        }
    }

    #[test]
    fn settings_schema_cache_is_usable_and_rejects_host_symlinks() {
        let profile = fixture("schemas");
        let root = profile.directory.join("generations/old/root");
        let schemas = root.join("usr/share/glib-2.0/schemas");
        fs::create_dir_all(&schemas).unwrap();
        fs::write(schemas.join("org.example.binfmt.gschema.xml"),
            "<schemalist><schema id='org.example.binfmt' path='/org/example/binfmt/'><key name='enabled' type='b'><default>true</default></key></schema></schemalist>").unwrap();
        compile_settings_schemas(&root).unwrap();
        let output = Command::new("/usr/bin/gsettings").args(["--schemadir", schemas.to_str().unwrap(), "get", "org.example.binfmt", "enabled"]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "true");
        fs::remove_file(schemas.join("gschemas.compiled")).unwrap();
        let outside = profile.directory.join("outside");
        fs::write(&outside, "untouched").unwrap();
        symlink(&outside, schemas.join("gschemas.compiled")).unwrap();
        assert!(compile_settings_schemas(&root).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "untouched");
        fs::remove_dir_all(profile.directory).unwrap();
    }

    #[test]
    fn dnf_and_image_failures_preserve_current_and_cleanup_staging() {
        for fail_program in ["/usr/bin/dnf5", "/usr/bin/mkfs.erofs"] {
            let profile = fixture(if fail_program.ends_with("dnf5") {
                "dnf-failure"
            } else {
                "image-failure"
            });
            let result = stage_generation(&profile, "install", &["hello".into()], |command| {
                if command.program == fail_program {
                    return Err("injected transaction failure".into());
                }
                if command.program == "/usr/bin/cp" {
                    return command.checked();
                }
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(
                fs::read_link(profile.directory.join("current")).unwrap(),
                Path::new("generations/old")
            );
            assert_eq!(
                fs::read(profile.directory.join("current/rootfs.erofs")).unwrap(),
                b"old image"
            );
            assert_eq!(
                fs::read_dir(profile.directory.join("generations"))
                    .unwrap()
                    .count(),
                1
            );
            fs::remove_dir_all(profile.directory).unwrap();
        }
    }

    #[test]
    fn successful_staging_publishes_new_image_and_retains_old_generation() {
        let profile = fixture("publish");
        let result =
            stage_generation(
                &profile,
                "install",
                &["hello".into()],
                |command| match command.program.as_str() {
                    "/usr/bin/cp" => command.checked(),
                    "/usr/bin/dnf5" => Ok(()),
                    "/usr/bin/mkfs.erofs" => {
                        fs::write(&command.args[5], "new image")?;
                        Ok(())
                    }
                    _ => panic!("unexpected command"),
                },
            )
            .unwrap();
        assert_eq!(
            profile.current().unwrap().file_name().unwrap(),
            result.as_str()
        );
        assert_eq!(
            fs::read(profile.directory.join("current/rootfs.erofs")).unwrap(),
            b"new image"
        );
        assert_eq!(
            fs::read(profile.directory.join("generations/old/rootfs.erofs")).unwrap(),
            b"old image"
        );
        fs::remove_dir_all(profile.directory).unwrap();
    }

    #[test]
    fn profile_lock_excludes_other_writers() {
        let profile = fixture("lock");
        let first = lock(&profile.directory).unwrap();
        assert!(lock(&profile.directory).is_err());
        drop(first);
        assert!(lock(&profile.directory).is_ok());
        fs::remove_dir_all(profile.directory).unwrap();
    }
}
