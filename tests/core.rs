use dnf_binfmt::{dnf_command, launch_command, Options, Profile};
use std::path::Path;

fn parse(args: &[&str]) -> dnf_binfmt::Result<Options> {
    Options::parse(args.iter().map(|s| s.to_string()).collect())
}
fn profile() -> Profile {
    Profile {
        directory: "/var/lib/dnf-binfmt/default".into(),
        releasever: "44".into(),
        overlays: vec![],
    }
}

#[test]
fn rejects_profile_traversal_and_host_root() {
    for name in ["../host", "/", "..", "a/b", "a\nb", "-option"] {
        assert!(parse(&["install", "--profile", name, "hello"]).is_err());
    }
    for path in ["/", "relative", "/var/../etc", "/var/lib\n/evil"] {
        assert!(parse(&["install", "--state-dir", path, "hello"]).is_err());
    }
}

#[test]
fn rejects_dnf_option_injection_even_after_separator() {
    assert!(parse(&["install", "--", "--installroot=/"]).is_err());
    assert!(parse(&["remove", "--", "--nogpgcheck"]).is_err());
}

#[test]
fn application_arguments_remain_literal() {
    let parsed = parse(&[
        "run",
        "--",
        "/usr/bin/app",
        "--profile",
        "space and 'quote'",
        "$(touch /tmp/evil)",
        "--help",
    ])
    .unwrap();
    assert_eq!(parsed.profile, "default");
    assert_eq!(parsed.args[1], "--profile");
    assert_eq!(parsed.args[3], "$(touch /tmp/evil)");
    let command = launch_command(&profile(), Path::new("/image.erofs"), &parsed.args);
    assert_eq!(
        &command.args[command.args.len() - parsed.args.len()..],
        parsed.args
    );
    assert!(command.args.contains(&"exec \"$@\"".to_string()));
    assert!(!command
        .args
        .iter()
        .any(|a| a.contains("touch") && a.contains("exec")));
}

#[test]
fn transaction_uses_private_root_and_disabled_scripts() {
    let command = dnf_command(
        &profile(),
        Path::new("/private/root"),
        "install",
        &["hello.x86_64".into()],
    );
    assert!(command
        .args
        .contains(&"--installroot=/private/root".to_string()));
    assert!(command.args.contains(&"--forcearch=x86_64".to_string()));
    assert!(command
        .args
        .contains(&"--setopt=tsflags=noscripts,notriggers,noplugins".to_string()));
    assert!(command.args.contains(&"--disable-plugin=*".to_string()));
    assert!(!command
        .args
        .iter()
        .any(|a| a == "--nogpgcheck" || a == "--installroot=/"));
}

#[test]
fn launch_uses_full_image_and_explicit_overlay_order() {
    let mut profile = profile();
    profile.overlays = vec!["/gpu-x86.erofs".into(), "/gpu-i386.erofs".into()];
    let command = launch_command(&profile, Path::new("/managed.erofs"), &["hello".into()]);
    let images: Vec<_> = command
        .args
        .windows(2)
        .filter(|a| a[0] == "-f")
        .map(|a| a[1].as_str())
        .collect();
    assert_eq!(
        images,
        ["/managed.erofs", "/gpu-x86.erofs", "/gpu-i386.erofs"]
    );
    assert!(!command.args.iter().any(|a| a.contains("default.erofs")));
}

#[test]
fn rejects_release_and_overlay_configuration_in_wrong_commands() {
    assert!(parse(&["init", "--releasever", "44\nplugins=1"]).is_err());
    assert!(parse(&["install", "--releasever", "44", "hello"]).is_err());
    assert!(parse(&["init", "--releasever", "44", "--overlay", "relative.erofs"]).is_err());
}

#[test]
fn malformed_command_is_an_error() {
    assert!(parse(&[]).is_err());
    assert!(parse(&["install"]).is_err());
    assert!(parse(&["install", "--profile"]).is_err());
    assert!(parse(&["does-not-exist"]).is_err());
}

#[test]
fn current_generation_cannot_escape_profile() {
    let root = std::env::temp_dir().join(format!("dnf-binfmt-test-{}", std::process::id()));
    std::fs::create_dir_all(root.join("generations")).unwrap();
    std::os::unix::fs::symlink("/tmp", root.join("current")).unwrap();
    let profile = Profile {
        directory: root.clone(),
        ..profile()
    };
    assert!(profile.current().is_err());
    std::fs::remove_dir_all(root).unwrap();
}
