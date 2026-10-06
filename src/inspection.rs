//! Capability detection uses ELF data, never application names or versions.
use crate::Result;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Debug)]
pub struct Inspection {
    pub elf_count: usize,
    pub desktop_gui: bool,
    pub needed: BTreeSet<String>,
    pub provided: BTreeSet<String>,
}

impl Inspection {
    pub fn missing(&self) -> Vec<String> {
        self.needed
            .difference(&self.provided)
            .map(|s| format!("{s}()(64bit)"))
            .collect()
    }
    pub fn gui(&self) -> bool {
        self.desktop_gui || self.needed.iter().any(|s| {
            s.starts_with("libgtk-")
                || s.starts_with("libgdk-")
                || s.starts_with("libQt5Gui.")
                || s.starts_with("libQt6Gui.")
                || s == "libflutter_linux_gtk.so"
        })
    }
    pub fn session_bus(&self) -> bool {
        self.gui()
            || self.needed.iter().any(|s| {
                s == "libsecret-1.so.0" || s == "libgio-2.0.so.0" || s.starts_with("libdbus-1.")
            })
    }
}

fn x86_64(header: &[u8]) -> bool {
    header.len() >= 20
        && &header[..4] == b"\x7fELF"
        && header[4] == 2
        && header[5] == 1
        && header[18..20] == [62, 0]
}

fn library_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
}

fn add_elf(path: &Path, original: &str, inspection: &mut Inspection) -> Result<()> {
    let output = Command::new("/usr/bin/readelf")
        .args(["--wide", "--dynamic", "--"])
        .arg(path)
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        return Err(format!("readelf could not inspect {}", path.display()).into());
    }
    inspection.elf_count += 1;
    if library_name(original) {
        inspection.provided.insert(original.into());
    }
    for line in String::from_utf8(output.stdout)?.lines() {
        let Some((_, value)) = line.split_once('[') else {
            continue;
        };
        let Some((value, _)) = value.split_once(']') else {
            continue;
        };
        if !library_name(value) {
            continue;
        }
        if line.contains("(NEEDED)") {
            inspection.needed.insert(value.into());
        }
        if line.contains("(SONAME)") {
            inspection.provided.insert(value.into());
        }
    }
    Ok(())
}

fn walk(path: &Path, inspection: &mut Inspection) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            walk(&entry?.path(), inspection)?;
        }
    } else if meta.is_file() {
        let mut header = [0; 20];
        let mut file = File::open(path)?;
        if file.read_exact(&mut header).is_ok() && x86_64(&header) {
            add_elf(
                path,
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref(),
                inspection,
            )?;
        }
    }
    Ok(())
}

pub fn tree(path: &Path) -> Result<Inspection> {
    let mut inspection = Inspection::default();
    walk(path, &mut inspection)?;
    inspection.desktop_gui = crate::desktop::has_graphical_launcher(path)?;
    Ok(inspection)
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn skip(reader: &mut impl Read, count: u64) -> Result<()> {
    if std::io::copy(&mut reader.take(count), &mut std::io::sink())? != count {
        return Err("truncated RPM payload".into());
    }
    Ok(())
}

fn field(header: &[u8], index: usize) -> Result<u64> {
    Ok(u64::from_str_radix(
        std::str::from_utf8(&header[6 + index * 8..14 + index * 8])?,
        16,
    )?)
}

pub fn rpm(path: &Path) -> Result<Inspection> {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "dnf-binfmt-elf-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    fs::DirBuilder::new().mode(0o700).create(&scratch.0)?;
    let mut child = Command::new("/usr/bin/rpm2cpio")
        .arg(path)
        .stdout(Stdio::piped())
        .spawn()?;
    let result: Result<Inspection> = (|| {
        let mut reader = child.stdout.take().ok_or("missing RPM payload stream")?;
        let mut inspection = Inspection::default();
        let mut total = 0_u64;
        let mut index = 0;
        loop {
            let mut header = [0_u8; 110];
            reader.read_exact(&mut header)?;
            if !matches!(&header[..6], b"070701" | b"070702") {
                return Err("unsupported RPM CPIO format".into());
            }
            let size = field(&header, 6)?;
            let namesize = field(&header, 11)?;
            let mode = field(&header, 1)?;
            total = total
                .checked_add(size + namesize + 110)
                .ok_or("RPM payload is too large")?;
            if namesize == 0
                || namesize > 4096
                || size > 512 * 1024 * 1024
                || total > 2 * 1024 * 1024 * 1024
            {
                return Err("RPM inspection size limit exceeded".into());
            }
            let mut name = vec![0; namesize as usize];
            reader.read_exact(&mut name)?;
            if name.last() != Some(&0) {
                return Err("invalid CPIO filename".into());
            }
            skip(&mut reader, (4 - (110 + namesize) % 4) % 4)?;
            let name = std::str::from_utf8(&name[..name.len() - 1])?;
            if name == "TRAILER!!!" {
                break;
            }
            let mut prefix = vec![0; size.min(20) as usize];
            reader.read_exact(&mut prefix)?;
            if mode & 0o170000 == 0o100000 && x86_64(&prefix) {
                // Never extract RPM paths or symlinks. Only isolated regular ELF
                // files with generated names are written for readelf to inspect.
                let file = scratch.0.join(format!("{index}.elf"));
                let mut output = File::create(&file)?;
                output.write_all(&prefix)?;
                if std::io::copy(
                    &mut reader.by_ref().take(size - prefix.len() as u64),
                    &mut output,
                )? != size - prefix.len() as u64
                {
                    return Err("truncated ELF payload".into());
                }
                add_elf(
                    &file,
                    name.rsplit('/').next().unwrap_or(name),
                    &mut inspection,
                )?;
                index += 1;
            } else {
                skip(&mut reader, size - prefix.len() as u64)?;
            }
            skip(&mut reader, (4 - size % 4) % 4)?;
        }
        // Drain block padding, otherwise rpm2cpio can block writing after trailer.
        std::io::copy(&mut reader.take(1024 * 1024), &mut std::io::sink())?;
        Ok(inspection)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    if !status.success() && result.is_ok() {
        return Err("rpm2cpio failed".into());
    }
    result
}

/// Resolve guest symlinks inside the managed root, including absolute links.
pub fn guest_path(root: &Path, executable: &str) -> Result<Option<PathBuf>> {
    let guesses: Vec<PathBuf> = if executable.starts_with('/') {
        vec![PathBuf::from(executable)]
    } else if executable.contains('/') {
        return Ok(None);
    } else {
        ["/usr/bin", "/bin", "/usr/local/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(executable))
            .collect()
    };
    for guess in guesses {
        let mut pending = guess
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(v) => Some(v.to_os_string()),
                std::path::Component::ParentDir => Some("..".into()),
                _ => None,
            })
            .collect::<std::collections::VecDeque<_>>();
        let mut relative = PathBuf::new();
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            if part == ".." {
                relative.pop();
                continue;
            }
            if part == "." {
                continue;
            }
            relative.push(&part);
            let candidate = root.join(&relative);
            if let Ok(meta) = fs::symlink_metadata(&candidate) {
                if meta.file_type().is_symlink() {
                    links += 1;
                    if links > 40 {
                        return Err("too many guest symlinks".into());
                    }
                    let target = fs::read_link(candidate)?;
                    relative.pop();
                    if target.is_absolute() {
                        relative.clear();
                    }
                    let components: Vec<_> = target
                        .components()
                        .filter_map(|c| match c {
                            std::path::Component::Normal(v) => Some(v.to_os_string()),
                            std::path::Component::ParentDir => Some("..".into()),
                            _ => None,
                        })
                        .collect();
                    for part in components.into_iter().rev() {
                        pending.push_front(part);
                    }
                }
            }
        }
        let path = root.join(relative);
        if path.is_file() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

pub fn application(root: &Path, executable: &str) -> Result<Inspection> {
    let Some(path) = guest_path(root, executable)? else {
        return Ok(Inspection::default());
    };
    let mut inspection = tree(&path)?;
    // Shell launchers/bootstrap RPMs do not contain the downloaded GUI ELF.
    // A matching non-terminal desktop entry still identifies a GUI launch.
    inspection.desktop_gui = crate::desktop::graphical_launcher(root, executable)?;
    // Bundled plugins may be dlopened, so also inspect an app's adjacent lib
    // directory. Do not scan every application in /usr/bin as one application.
    if let Some(parent) = path.parent() {
        let libraries = parent.join("lib");
        if libraries.is_dir() && !fs::symlink_metadata(&libraries)?.file_type().is_symlink() {
            walk(&libraries, &mut inspection)?;
        }
    }
    Ok(inspection)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_are_library_based_and_cli_libraries_do_not_trigger_gui() {
        let mut inspection = Inspection::default();
        inspection.needed.insert("libc.so.6".into());
        assert!(!inspection.gui() && !inspection.session_bus());
        inspection.needed.insert("libsecret-1.so.0".into());
        assert!(!inspection.gui() && inspection.session_bus());
        inspection.needed.insert("libflutter_linux_gtk.so".into());
        assert!(inspection.gui());
        inspection.provided.insert("libflutter_linux_gtk.so".into());
        assert!(!inspection
            .missing()
            .contains(&"libflutter_linux_gtk.so()(64bit)".into()));
    }

    #[test]
    fn guest_absolute_and_relative_symlinks_stay_in_managed_root() {
        let root = std::env::temp_dir().join(format!(
            "dnf-binfmt-links-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        fs::create_dir_all(root.join("usr/share/example")).unwrap();
        fs::write(root.join("usr/share/example/app"), "payload").unwrap();
        std::os::unix::fs::symlink("/usr/share/example/app", root.join("usr/bin/app")).unwrap();
        std::os::unix::fs::symlink("../share/example/app", root.join("usr/bin/relative-app"))
            .unwrap();
        assert_eq!(
            guest_path(&root, "app").unwrap().unwrap(),
            root.join("usr/share/example/app")
        );
        assert_eq!(
            guest_path(&root, "relative-app").unwrap().unwrap(),
            root.join("usr/share/example/app")
        );
        std::os::unix::fs::symlink("/etc/passwd", root.join("usr/bin/outside")).unwrap();
        assert!(guest_path(&root, "outside").unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
