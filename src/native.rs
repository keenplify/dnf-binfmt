use crate::{
    environment::{dnf_command, CommandSpec, Profile},
    Result,
};
use std::{collections::BTreeMap, path::Path, process::Command};

const FORMAT: &str = "%{name}\t%{epoch}\t%{version}\t%{release}\t%{arch}\n";

#[derive(Debug)]
struct Package {
    name: String,
    evr: String,
    arch: String,
}

fn valid(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".+_-~^:".contains(&c))
}

fn records(content: &str) -> Result<Vec<Package>> {
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 5 {
                return Err("unexpected package query output".into());
            }
            let epoch = match fields[1] {
                "(none)" | "" => "0",
                value => value,
            };
            if !epoch.bytes().all(|c| c.is_ascii_digit())
                || ![fields[0], fields[2], fields[3], fields[4]]
                    .iter()
                    .all(|v| valid(v))
            {
                return Err("invalid package metadata in native-build check".into());
            }
            Ok(Package {
                name: fields[0].into(),
                evr: format!("{epoch}:{}-{}", fields[2], fields[3]),
                arch: fields[4].into(),
            })
        })
        .collect()
}

pub fn query(profile: &Profile, root: &Path, architecture: &str) -> CommandSpec {
    let mut spec = dnf_command(profile, root, "repoquery", &[]);
    for arg in &mut spec.args {
        if arg == "--forcearch=x86_64" {
            *arg = format!("--forcearch={architecture}");
        }
        for key in ["logdir", "cachedir", "system_cachedir"] {
            if arg.starts_with(&format!("--setopt={key}=")) {
                *arg = format!(
                    "--setopt={key}={}/native-check/{architecture}/{key}",
                    profile.directory.display()
                );
            }
        }
    }
    let separator = spec.args.len() - 1;
    spec.args.splice(
        separator..separator,
        [
            "--available".into(),
            format!("--arch={architecture}"),
            "--latest-limit=1".into(),
            format!("--queryformat={FORMAT}"),
        ],
    );
    spec
}

fn compare(left: &str, right: &str) -> Result<i32> {
    // Restricted EVR characters cannot escape the Lua literals or expand RPM
    // macros. rpm.vercmp preserves epochs, releases, tilde, and caret semantics.
    if !valid(left) || !valid(right) {
        return Err("invalid EVR".into());
    }
    let expression = format!("%{{lua: print(rpm.vercmp(\"{left}\", \"{right}\"))}}");
    let output = Command::new("/usr/bin/rpm")
        .args(["--eval", &expression])
        .output()?;
    if !output.status.success() {
        return Err("RPM version comparison failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

pub fn check(profile: &Profile, root: &Path) -> Result<()> {
    let architecture = std::env::consts::ARCH;
    if architecture != "aarch64" {
        return Err("native-build checks currently target aarch64 hosts".into());
    }
    let installed = Command::new("/usr/bin/rpm")
        .args(["--root"])
        .arg(root)
        .args([
            "-qa",
            "--queryformat",
            "%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n",
        ])
        .output()?;
    if !installed.status.success() {
        return Err("could not query the compatibility RPM database".into());
    }
    let installed = records(&String::from_utf8(installed.stdout)?)?
        .into_iter()
        .filter(|p| p.arch == "x86_64")
        .collect::<Vec<_>>();
    if installed.is_empty() {
        return Ok(());
    }
    let mut query = query(profile, root, architecture);
    query.args.extend(installed.iter().map(|p| p.name.clone()));
    eprintln!("Checking native {architecture} builds before upgrading x86-64 packages...");
    let output = query.process().output()?;
    if !output.status.success() {
        return Err(format!(
            "native-build query failed; upgrade was not started: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let available = records(&String::from_utf8(output.stdout)?)?;
    let mut native = BTreeMap::new();
    for package in available {
        if package.arch != architecture {
            continue;
        }
        if let Some(existing) = native.get(&package.name) {
            let existing: &Package = existing;
            if compare(&package.evr, &existing.evr)? <= 0 {
                continue;
            }
        }
        native.insert(package.name.clone(), package);
    }
    let mut count = 0;
    for package in installed {
        if let Some(candidate) = native.get(&package.name) {
            let relation = compare(&candidate.evr, &package.evr)?;
            println!(
                "Native build: {}.{} {} ({} installed x86-64 {})",
                candidate.name,
                candidate.arch,
                candidate.evr,
                if relation > 0 {
                    "newer than"
                } else if relation == 0 {
                    "same version as"
                } else {
                    "older than"
                },
                package.evr
            );
            count += 1;
        }
    }
    println!("Native check: {count} replacement candidates, including dependencies. This reports same-name packages; it does not install them into the host or migrate app data.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_query_uses_host_architecture_and_private_caches() {
        let profile = Profile {
            directory: "/var/lib/dnf-binfmt/default".into(),
            releasever: "44".into(),
            overlays: vec![],
        };
        let spec = query(&profile, Path::new("/private/root"), "aarch64");
        assert!(spec.args.contains(&"--forcearch=aarch64".into()));
        assert!(spec.args.contains(&"--arch=aarch64".into()));
        assert!(!spec.args.contains(&"--forcearch=x86_64".into()));
        assert!(spec
            .args
            .iter()
            .any(|a| a.contains("native-check/aarch64/logdir")));
    }
    #[test]
    fn rpm_versions_use_epoch_and_rpm_ordering() {
        assert!(compare("0:4.4.26-1", "0:4.4.25-1").unwrap() > 0);
        assert!(compare("0:4.4.25~rc1-1", "0:4.4.25-1").unwrap() < 0);
        assert!(compare("1:1.0-1", "0:99.0-1").unwrap() > 0);
        assert!(compare("0:1.0-99", "0:1.0.1-1").unwrap() < 0);
        assert!(compare("0:1.0\")) print(1)", "0:1.0").is_err());
    }
}
