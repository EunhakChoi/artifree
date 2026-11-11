//! Keeps the Linux server binary loadable on the supported glibc floor
//! (decision 0037: 2.34) now that it starts a child process (decision 0055).
//!
//! The standard library's process spawning refers weakly to `pidfd_spawnp` and
//! `pidfd_getpid`, which glibc added in 2.39, and uses them only when they are
//! present. A release built on a machine with glibc 2.39 still records that
//! version as required, and neither GNU ld nor LLD marks the requirement as
//! weak, so an older glibc refuses to load the binary. Defining both symbols
//! as absent at link time removes the requirement; the standard library then
//! takes the path it takes on every older glibc. `scripts/lib/elf-glibc.mjs`
//! fails the build if a requirement above the floor comes back, and the Debian
//! 12 job runs the archive.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let is = |name: &str, value: &str| env::var(name).is_ok_and(|found| found == value);
    if is("CARGO_CFG_TARGET_OS", "linux") && is("CARGO_CFG_TARGET_ENV", "gnu") {
        for symbol in ["pidfd_spawnp", "pidfd_getpid"] {
            println!("cargo:rustc-link-arg-bins=-Wl,--defsym={symbol}=0");
        }
    }
}
