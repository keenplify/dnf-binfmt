# dnf-binfmt

An experimental Rust backend and DNF5 command plugin for installing x86-64
RPM applications into a separate environment on **Fedora Asahi Remix** and
launching them through **muvm + FEXBash**.

```sh
sudo dnf binfmt init --releasever 44
sudo dnf binfmt install --accept-no-scripts hello.x86_64
dnf binfmt run -- /usr/bin/hello
```

Replace `44` with the Fedora release you intend to use. The environment release
is pinned at initialization; `upgrade` updates packages within that release.

This project is independent of Fedora, Asahi Linux, and FEX. The repository is
MIT licensed and initialized with Git; it has not been published to a remote.

## Status and limitations

This is a first prototype, **not a production compatibility layer**. Rust and
the DNF5 adapter compile, and the plugin command is exercised with the real
DNF5 parser. Tests check argument isolation and launch/image planning.
Failure tests check that unsuccessful staging preserves the active generation.
Full DNF installation and application execution on Apple Silicon still need
validation. See [VALIDATION.md](VALIDATION.md) for the exact checks performed.

**All RPM scripts and triggers are skipped**, including dependency scripts.
Every install, upgrade, and removal therefore requires `--accept-no-scripts`.
Apps relying on setup scripts, caches, services, kernel modules, or host
integration may fail. Implementing scripts inside an emulated writable root
is the main next step. Running them on the ARM host would not fix this.

The installed root becomes a complete read-only EROFS image. The runtime is
not a security sandbox: muvm exposes host filesystem/session resources, and
applications run as the launching user. The image does not offer a writable
guest `/etc`, `/var`, or `/opt`; applications needing these paths require more
work. Per-user application settings can use the user's home directory.

## Build and install

Requirements: Fedora's Rust/Cargo (Rust 1.89+), C++20 compiler, Make,
`dnf5-devel`, `libdnf5-devel`, `libdnf5-cli-devel`, and `fmt-devel`.
The adapter targets DNF5 plugin API 2.0 and libdnf5-cli ABI 3.

```sh
sudo dnf install rust cargo gcc-c++ make dnf5-devel libdnf5-devel libdnf5-cli-devel fmt-devel
cargo test --offline
make
sudo make install
sudo dnf install binfmt-dispatcher fex-emu muvm erofs-utils binutils rpm-build python3 xdg-dbus-proxy
dnf binfmt doctor
```

`make install` installs the backend to `/usr/libexec/dnf-binfmt` and the plugin
to `/usr/lib64/dnf5/plugins/binfmt.so`. It does not enable any global foreign
architecture settings. `dnf` must resolve to DNF5; DNF4 is not supported.
The Rust crate has no external dependencies.

Use `make install DESTDIR=/path/to/staging` for packaging. `PREFIX` and
`LIBDIR` are overridable; rebuild the plugin if changing the backend path.
No RPM spec or published RPM repository is provided yet.

## Commands

Initialize the managed environment once, then use package names or local RPMs:

```sh
sudo dnf binfmt init --releasever 44
sudo dnf binfmt install --accept-no-scripts hello.x86_64
sudo dnf binfmt install --accept-no-scripts ./application.x86_64.rpm
dnf binfmt list
dnf binfmt run -- /usr/bin/hello
dnf binfmt export
sudo dnf binfmt upgrade --accept-no-scripts
sudo dnf binfmt remove --accept-no-scripts hello
dnf binfmt export
```

Run applications and `export` as your desktop user. Export discovers normal
`.desktop` files in the environment's `/usr/share/applications`, rewrites
their launch commands, preserves file/URL field codes and desktop actions,
and removes stale launchers belonging to that profile. D-Bus activation and
host `TryExec`/working-directory checks are disabled. Icons and requested
working directories are not integrated yet. Refresh exports after package
changes; exporting is not automatically performed by root installations.

Use `--profile NAME` after a subcommand to manage additional environments:

```sh
sudo dnf binfmt init --profile tools --releasever 44
sudo dnf binfmt install --profile tools --accept-no-scripts hello.x86_64
dnf binfmt run --profile tools -- /usr/bin/hello --help
```

Add `--dry-run` to print the plan without writes or process execution. Local
RPM dry runs query their metadata with RPM but do not install them. Plans
require an initialized profile except for `init --dry-run`, `inspect`, and `doctor`.

```sh
dnf binfmt init --releasever 44 --dry-run
dnf binfmt install --accept-no-scripts --dry-run hello.x86_64
dnf binfmt run --dry-run -- /usr/bin/hello
```

Global DNF options (such as `-y`, `--repo`, `--nogpgcheck`, and `--installroot`)
are not forwarded to the backend transaction. The private DNF process prompts
normally. Put supported binfmt options after the subcommand. Use `--` before
application flags. The standalone `dnf-binfmt` binary accepts the same commands.

## Global compatibility policies

These policies apply by detected capabilities, without application-name recipes.
`dnf binfmt inspect ./application.x86_64.rpm` streams the RPM payload and reports
x86-64 ELF files, external library requirements, and GUI/session-bus detection.
It does not execute the application or extract paths from the archive.

Installation requests providers for missing ELF SONAMEs, then audits the staged
root and resolves missing providers before publishing the image. Bundled
libraries are taken into account. This supplements RPM dependency metadata;
optional ELF plugins can pull in unnecessary libraries, and libraries loaded
only by name through `dlopen` cannot all be discovered this way.

GTK, Qt GUI, and Flutter GTK applications default to software rendering and X11
settings to avoid the observed accelerated-rendering failure. Applications
using GUI/session-bus libraries get a per-launch filtered bridge. It allows
Secret Service, tray registration, notifications, and desktop portals. Exact
application bus identities are inferred from matching desktop entries, with a
narrow tray-item ownership rule. Arbitrary host bus services remain inaccessible.
The bridge authenticates with a private nonce and adapts GLib authentication to
a filtered host connection. The small bridge helper uses Python's standard
library and `xdg-dbus-proxy`; package management and policy remain Rust.

Overrides are available for applications whose capabilities cannot be detected:

```sh
dnf binfmt run --graphics software --session-bus filtered -- /usr/bin/application
dnf binfmt run --graphics accelerated --session-bus off -- /usr/bin/application
```

`--graphics` accepts `auto`, `software`, or `accelerated`; `--session-bus`
accepts `auto`, `filtered`, or `off`. Defaults are `auto`. Keyring authorization
and prompts still belong to the host keyring service. The bridge is session
integration, not a general security sandbox.

Before `upgrade`, the backend queries the configured repositories for same-name
**aarch64** versions of installed x86-64 packages. It compares epoch, version,
and release using RPM semantics and reports whether the native candidate is
newer, equal, or older. It does not automatically migrate installations or user
data. Repository query failure stops the upgrade so the check is not silently
skipped. A vendor that publishes ARM64 builds only outside configured RPM
repositories cannot be discovered yet.

New profiles use `$basearch` repository URLs so the same configuration serves
both architecture queries. Profiles from an earlier prototype with fixed
`x86_64` URLs need those URLs changed to `$basearch` before native checks can
find ARM64 packages.

## How it builds on binfmt-dispatcher

[binfmt-dispatcher](https://github.com/AsahiLinux/binfmt-dispatcher) already
selects the interpreter for executable files and can route execution through
muvm. It expects the executable on kernel-provided file descriptor 3, so this
project does **not** invoke it like a normal launcher or replace its handlers.

The new layer manages RPM installation and dependencies. It launches the
managed rootfs through muvm's existing FEX integration; muvm configures its
guest binfmt handlers. Explicit `FEXBash` launch lets commands resolve against
the managed x86-64 rootfs even when the executable is absent on the ARM host.

Each generation and overlay configuration uses a private muvm runtime directory
under `$XDG_RUNTIME_DIR/dnf-binfmt/`. muvm otherwise reuses the user's existing
VM before processing `-f`, which can silently select a different rootfs and
report a managed executable as missing. The launcher also sets
`FEX_ROOTFS=/run/fex-emu/rootfs` inside the VM so user FEX configuration cannot
select an unrelated image. Host audio socket discovery is preserved.

An installed guest command such as `/usr/bin/hello` is stored in the managed
root, so invoking `/usr/bin/hello` directly on the ARM host can still report
“No such file or directory”. Use `dnf binfmt run -- /usr/bin/hello`.

```text
dnf binfmt install PACKAGE
  -> DNF5 command adapter
  -> Rust backend
  -> DNF5 --forcearch=x86_64 --installroot=<staged-root>
  -> mkfs.erofs -b4096
  -> publish complete generation atomically

dnf binfmt run -- COMMAND ARGS
  -> muvm --emu=fex -f <generation-image> -- FEXBash
  -> x86-64 command in the managed rootfs
```

Upstream references:

- [muvm runtime and image interface](https://github.com/AsahiLinux/muvm)
- [DNF5 installroot](https://dnf5.readthedocs.io/en/stable/misc/installroot.7.html)
- [DNF5 architecture selection](https://dnf5.readthedocs.io/en/stable/misc/forcearch.7.html)
- [DNF5 plugins](https://dnf5.readthedocs.io/en/stable/tutorial/plugins/)

No upstream source is vendored or patched.

## Package state and repositories

Profiles live under `/var/lib/dnf-binfmt/NAME`:

```text
profile.conf              Pinned release and extra image paths
dnf.conf                  Separate configuration; signatures enabled
fedora-key.asc            Copied Fedora release signing key
repos.d/fedora.repo       Fedora and updates with $basearch URLs
generations/<id>/root/    Complete RPM-managed filesystem and database
generations/<id>/rootfs.erofs
current -> generations/<id>
```

The first installation includes x86-64 bash, glibc, and filesystem packages.
DNF resolves dependencies using its own database in the managed root; it does
not assume the host's libraries or shared FEX rootfs satisfy those dependencies.
The default repository set excludes Asahi-only and host third-party repos.
Add vendor repositories/signing keys to a profile as an administrator if needed.
Local RPM signatures are checked too; unsigned local RPMs are not accepted by
the default configuration. Import required vendor keys into the managed RPM
database/configuration before installing their packages.

Mutations copy the previous generation, run DNF in the copy, build the image,
and publish the `current` symlink only after success. A profile lock serializes
writers. Existing generations remain available to running applications.
There is no rollback command or garbage collection yet: repeated transactions
consume disk space. Failed staging is removed; interrupted runs may leave an
unpublished generation for administrative cleanup. This is failure isolation,
not a guarantee against a power failure during copying or filesystem corruption.

A complete image is used instead of overlaying a shared base: removed packages
must not become visible again through a lower image layer. Extra GPU EROFS
images can be configured with repeated `init --overlay /absolute/path` options.
They are mounted after the managed image; use compatible Fedora/Asahi images.
GPU thunking and overlay compatibility have not been validated here.

MIT licensed. Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).
