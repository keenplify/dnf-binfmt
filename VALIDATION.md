# Validation

Checked on 2026-10-07 in an aarch64 Fedora environment with 16,384-byte pages.
DNF5 5.4.6.0 (plugin API 2.0), RPM 6.0.2, Rust/Cargo 1.98.1.

Passed:

- Release build of the Rust backend.
- C++20 plugin compilation with `-Wall -Wextra -Werror`.
- `make install DESTDIR=<workspace-stage>` with the production backend path;
  backend, plugin, session bridge, native/guest compatibility helpers, and
  license installed into the staging directory only.
- 25 Rust tests: profile/path validation, package-option injection, literal
  application arguments, private transaction roots, complete-image launching,
  overlay ordering, desktop entries, failed DNF/image staging, publication,
  retained old images, exclusive profile locking, ELF capability detection, guest symlink resolution,
  generic runtime policies, desktop bus identities, native-query isolation,
  RPM epoch/version/release comparisons, generation/overlay-specific VM reuse,
  and private runtime directory permission/symlink checks.
- Seven Python bridge tests, including fragmented authentication, nonce rejection,
  literal command arguments, exact identity filters, and a real GLib client
  through nonce TCP and xdg-dbus-proxy to an isolated D-Bus daemon.
- Inspection of the supplied Ente Auth 4.4.25 RPM: 16 x86-64 ELF files,
  GUI/session-bus detection, and external SONAME requirements. No Ente-specific
  rules or application payload are included in this repository.
- Earlier Clippy check on all targets with warnings denied; Clippy is unavailable
  in the current toolchain, so it was not rerun for the latest changes.
- Actual DNF5 plugin registration, subcommand help, and backend invocation.
- `tests/smoke.py`: real DNF5 argument parsing, install/run/upgrade dry-run plans, generic software/filtered launch policy,
  local RPM inspection,
  transactions without skipped-script acknowledgement and legacy flag compatibility, invalid-RPM rejection, real EROFS creation
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

A real privileged DNF transaction was successful in the supplied host log;
it was not repeated in this workspace. Actual `hello` launches through the
managed image succeeded outside the filesystem/process sandbox.

The native mmap helper passed four regression tests. The guest x86 helper
passed the same four tests under FEX 2604 in muvm: a reservation exceeding
4 GiB with a low hint could be unmapped, fixed mappings retained their address,
syscall errors preserved errno, and `mmap64` shared the compatibility entry
point.

Discord 1.0.160 completed updates, loaded its web UI, and reported
`renderer-full-interactive` through the rebuilt backend, filtered session bridge,
and Fedora's installed FEX 2604. The cold startup took about 213 seconds.
It used software graphics, `--disable-gpu`, and `--no-sandbox`. Temporary
read-only overlays supplied the missing X11 and Mesa runtime packages; the
GUI transaction audit now installs those providers and the common GTK/GBM/audio
ABI automatically. Network-service timeouts and restarts occurred during
startup but recovered. An isolated official FEX 2609.1 test also loaded the
Discord web UI. No newer emulator was installed on the host.
A later repeat launch stalled on the splash screen. Verbose logs showed the
network child terminating after its 15-second IPC connection deadline. With
`--ipc-connection-timeout=180`, the installed package root reached
`renderer-full-interactive` after about 199 seconds and connected to Discord's
gateway. The launcher now supplies that timeout for `/usr/bin/discord` and
`discord`, while preserving caller overrides. Disabling the session bridge
and testing portable FEX 2609.1 did not remove the original timeout. Discord
rewrites its Chromium feature switches, so the in-process network feature
experiment did not take effect. The GUI dependency audit now also installs
Fedora's separate `gtk3-immodule-xim` package.
Existing-session login resumed during this test. Audio reported a missing ALSA
PipeWire module; audio, video, and screen sharing remain unverified.

Not yet verified:

- Generic graphical application launch, host keyring integration in muvm,
  GPU overlays/thunks, and runtime performance.
- Full managed-root native update checks against live vendor repositories.
- Vendor package setup and packages requiring installation scripts.

The restricted tool sandbox hides `/dev/kvm`; approved host-side commands can
access it and were used for the live VM checks. Debugging and cross-assembler
RPMs were downloaded and extracted into `/tmp`; no host package installation
was performed by the agent. Installing the rebuilt backend/plugin still needs
`sudo make install`. The local repository
was initialized with `git init -b main`; no remote or initial commit was created.
