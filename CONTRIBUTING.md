# Contributing

This is an independent experimental project. It is not an official Fedora,
Asahi Linux, FEX, or binfmt-dispatcher component.

Use Rust for environment management and keep the C++ DNF5 adapter small.
Run `cargo test --offline` and `make` before submitting changes. Test runtime
changes on Fedora Asahi Remix with KVM and report the Fedora release, muvm,
FEX, and DNF5 versions, the exact command, and application results.

Preserve separate package databases, signature checks, argument boundaries,
and atomic generation publication. Failed staging must leave the previous
generation usable. Do not add host-wide architecture overrides or rewrite
existing binfmt handlers.

Priority work: run RPM scripts inside a writable emulated environment; test
real package transactions and launches; integrate GPU overlays, icons, and
working directories; add an RPM spec and CI; implement rollback and garbage
collection. DNF4 support needs a separate adapter.

Contributions are licensed under the project's MIT license.
