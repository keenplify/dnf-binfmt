#!/usr/bin/env python3
"""Exercise real DNF5 parsing and EROFS tools; never install packages or launch VMs.

Build the test adapter with BACKEND set to the absolute release binary path:
  make -B build/binfmt.so BACKEND="$PWD/target/release/dnf-binfmt"
  python3 tests/smoke.py
"""
import argparse
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--plugin", type=Path, default=Path("build/binfmt.so"))
parser.add_argument("--backend", type=Path, default=Path("target/release/dnf-binfmt"))
parser.add_argument("--rpm", type=Path, help="Optional local RPM regression input; inspected only")
args = parser.parse_args()
plugin, backend = args.plugin.resolve(), args.backend.resolve()


def checked(command, env, expect=0):
    result = subprocess.run(command, env=env, capture_output=True, text=True)
    if result.returncode != expect:
        raise AssertionError(f"{command}: expected {expect}, got {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout + result.stderr


with tempfile.TemporaryDirectory(prefix="dnf-binfmt-smoke-") as temporary:
    scratch = Path(temporary)
    plugins = scratch / "plugins"
    plugins.mkdir()
    for installed in Path("/usr/lib64/dnf5/plugins").glob("*.so"):
        if installed.name != plugin.name:
            (plugins / installed.name).symlink_to(installed)
    (plugins / "binfmt.so").symlink_to(plugin)
    environment = dict(os.environ, DNF5_PLUGINS_DIR=str(plugins), XDG_DATA_HOME=str(scratch / "data"))
    (scratch / "logs").mkdir()
    dnf = ["/usr/bin/dnf5", f"--setopt=logdir={scratch / 'logs'}", "binfmt"]

    assert "install" in checked(dnf + ["--help"], environment)
    assert "Create profile" in checked(dnf + ["init", "--releasever=44", "--dry-run"], environment)

    state = scratch / "state"
    profile = state / "default"
    generation = profile / "generations/fixture"
    root = generation / "root"
    (root / "usr/bin").mkdir(parents=True)
    (root / "usr/bin/bash").write_text("Synthetic test payload; not an executable.\n")
    (root / "var/cache").mkdir(parents=True)
    (root / "var/cache/secret").write_text("Must not be in the image")
    applications = root / "usr/share/applications"
    applications.mkdir(parents=True)
    (applications / "example.desktop").write_text("[Desktop Entry]\nType=Application\nName=Example\nExec=/usr/bin/example %U\nTryExec=/usr/bin/example\nDBusActivatable=true\n")
    (profile / "profile.conf").write_text("releasever=44\n")
    (profile / "current").symlink_to("generations/fixture")
    image = generation / "rootfs.erofs"

    checked(["/usr/bin/mkfs.erofs", "-b4096", "-zlz4", "--all-root", "--exclude-path=var/cache", "--exclude-path=var/log", str(image), str(root)], environment)
    extracted = scratch / "extracted"
    checked(["/usr/bin/fsck.erofs", f"--extract={extracted}", str(image)], environment)
    assert (extracted / "usr/bin/bash").read_bytes() == (root / "usr/bin/bash").read_bytes()
    assert not (extracted / "var/cache/secret").exists()

    before = image.read_bytes()
    plan = checked(dnf + ["install", f"--state-dir={state}", "--dry-run", "hello.x86_64"], environment)
    assert "--forcearch=x86_64" in plan and "--installroot=" in plan
    assert "noscripts,notriggers,noplugins" in plan
    assert image.read_bytes() == before
    legacy_plan = checked(dnf + ["install", f"--state-dir={state}", "--accept-no-scripts", "--dry-run", "hello.x86_64"], environment)
    assert legacy_plan == plan
    removal = checked(dnf + ["remove", f"--state-dir={state}", "--dry-run", "hello.x86_64"], environment)
    assert "noscripts,notriggers,noplugins" in removal

    launch = checked(dnf + ["run", f"--state-dir={state}", "--dry-run", "--", "/usr/bin/example", "--help", "literal $(touch nope)"], environment)
    assert "FEXBash" in launch and "literal $(touch nope)" in launch
    assert "XDG_RUNTIME_DIR=" in launch and "/dnf-binfmt/" in launch
    assert "FEX_ROOTFS=/run/fex-emu/rootfs" in launch
    assert "--help" in launch
    assert not Path("nope").exists()
    software = checked(dnf + ["run", f"--state-dir={state}", "--dry-run", "--graphics=software", "--session-bus=filtered", "--", "/usr/bin/example"], environment)
    assert "LIBGL_ALWAYS_SOFTWARE=1" in software and "session_bus.py" in software
    assert "XDG_RUNTIME_DIR=" in software and "/dnf-binfmt/" in software
    update = checked(dnf + ["upgrade", f"--state-dir={state}", "--dry-run"], environment)
    assert "--forcearch=aarch64" in update and "--arch=aarch64" in update
    assert image.read_bytes() == before

    if args.rpm:
        inspected = checked(dnf + ["inspect", str(args.rpm.resolve())], environment)
        assert "x86-64 ELF files:" in inspected and "External library requirements" in inspected

    bad_rpm = scratch / "invalid.x86_64.rpm"
    bad_rpm.write_text("This is not an RPM.")
    output = checked([str(backend), "install", "--state-dir", str(state), "--dry-run", str(bad_rpm)], environment, expect=1)
    assert "not an x86_64/noarch RPM" in output

    if os.geteuid() != 0:
        destination = scratch / "data/applications"
        destination.mkdir(parents=True)
        stale = destination / "dnf-binfmt-default.stale.desktop"
        stale.write_text("stale")
        other_profile = destination / "dnf-binfmt-default-other.keep.desktop"
        other_profile.write_text("different profile")
        checked(dnf + ["export", f"--state-dir={state}"], environment)
        launcher = destination / "dnf-binfmt-default.example.desktop"
        command = shlex.split(next(line[5:] for line in launcher.read_text().splitlines() if line.startswith("Exec=")))
        assert command[1] == "run"
        assert command[-3:] == ["--", "/usr/bin/example", "%U"]
        # Execute the generated argument order through the actual backend parser.
        checked(command[:2] + ["--dry-run"] + command[2:], environment)
        assert not stale.exists() and other_profile.exists()
        validator = shutil.which("desktop-file-validate")
        if validator:
            checked([validator, str(launcher)], environment)
    assert image.read_bytes() == before
    assert (profile / "current").readlink() == Path("generations/fixture")

print("PASS: real DNF5 plugin parsing, install/run plans, argument forwarding, RPM rejection, EROFS integrity, launcher export, and profile isolation.")
print("No RPM transaction or muvm application launch was performed.")
