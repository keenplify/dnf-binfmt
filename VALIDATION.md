# Validation

Checked on 2026-10-06 in an aarch64 Fedora environment with 16,384-byte pages.
DNF5 5.4.6.0 (plugin API 2.0), RPM 6.0.2, Rust/Cargo 1.98.1.

Passed:

- Release build of the Rust backend.
- C++20 plugin compilation with `-Wall -Wextra -Werror`.
- `make install DESTDIR=<workspace-stage>` with the production backend path;
  backend, plugin, and license installed into the staging directory only.
- 20 Rust tests: profile/path validation, package-option injection, literal
  application arguments, private transaction roots, complete-image launching,
  overlay ordering, desktop entries, failed DNF/image staging, publication,
  retained old images, exclusive profile locking, ELF capability detection, guest symlink resolution,
  generic runtime policies, desktop bus identities, native-query isolation,
  and RPM epoch/version/release comparisons.
- Seven Python bridge tests, including fragmented authentication, nonce rejection,
  literal command arguments, exact identity filters, and a real GLib client
  through nonce TCP and xdg-dbus-proxy to an isolated D-Bus daemon.
- Inspection of the supplied Ente Auth 4.4.25 RPM: 16 x86-64 ELF files,
  GUI/session-bus detection, and external SONAME requirements. No Ente-specific
  rules or application payload are included in this repository.
- Clippy on all targets with warnings denied.
- Actual DNF5 plugin registration, subcommand help, and backend invocation.
- `tests/smoke.py`: real DNF5 argument parsing, install/run/upgrade dry-run plans, generic software/filtered launch policy,
  local RPM inspection,
  skipped-script acknowledgement, invalid-RPM rejection, real EROFS creation
  with 4K blocks and LZ4, integrity/extraction checks, cache exclusion, actual
  desktop export and validation, stale-launcher cleanup, and isolation from
  other profiles.

Run the smoke test from the repository after building the backend:

```sh
make -B build/binfmt.so BACKEND="$PWD/target/release/dnf-binfmt"
python3 tests/smoke.py
python3 -m unittest discover -s tests -p 'test_*.py' -v
```

The smoke test uses temporary state, restores nothing because it changes no
host package state, and never performs a package installation or VM launch.
The transaction-failure tests inject child-process failures; they do not
simulate every RPM failure. Desktop-export checks run under a non-root user.

Not yet verified:

- A real privileged DNF transaction in the managed installroot.
- Running the resulting image/application through muvm + FEX on Apple Silicon.
- Generic graphical application launch, host keyring integration in muvm,
  GPU overlays/thunks, and runtime performance.
- Full managed-root native update checks against live vendor repositories.
- Vendor package setup and packages requiring installation scripts.

This session has no `/dev/kvm`. `doctor` correctly reports that limitation.
Development RPMs were downloaded and extracted into the workspace; no build
dependencies or plugin were installed into the host. The local repository
was initialized with `git init -b main`; no remote or initial commit was created.
