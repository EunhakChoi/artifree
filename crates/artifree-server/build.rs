//! Keeps the Linux server binary loadable on the supported glibc floor
//! (decision 0037: 2.34) now that it starts a child process (decision 0055).
//!
//! The standard library's process spawning refers weakly to `pidfd_spawnp` and
//! `pidfd_getpid`, which glibc added in 2.39, and uses them only when they are
//! present. A release built on a machine with glibc 2.39 still records that
//! version as required, and neither GNU ld nor LLD marks the requirement as
//! weak (checked with both), so an older glibc refuses to load the binary.
//! Defining both symbols as absent at link time removes the requirement; the
//! standard library then takes the path it takes on every older glibc. A
//! version script keeps the two definitions local, so they are not in the
//! dynamic symbol table and no shared library can resolve to them.
//! `scripts/lib/elf-glibc.mjs` fails the build if a requirement above the
//! floor comes back, and the Debian 12 job runs the archive.

use std::path::PathBuf;
use std::{env, fs, io};

/// The glibc 2.39 symbols the standard library refers to weakly.
const SYMBOLS: [&str; 2] = ["pidfd_spawnp", "pidfd_getpid"];

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    let is = |name: &str, value: &str| env::var(name).is_ok_and(|found| found == value);
    if !(is("CARGO_CFG_TARGET_OS", "linux") && is("CARGO_CFG_TARGET_ENV", "gnu")) {
        return Ok(());
    }
    let out_dir = env::var_os("OUT_DIR")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cargo did not set OUT_DIR"))?;
    let script = PathBuf::from(out_dir).join("absent-symbols.map");
    fs::write(&script, format!("{{ local: {}; }};\n", SYMBOLS.join("; ")))?;
    for symbol in SYMBOLS {
        println!("cargo:rustc-link-arg-bins=-Wl,--defsym={symbol}=0");
    }
    println!(
        "cargo:rustc-link-arg-bins=-Wl,--version-script={}",
        script.display()
    );
    Ok(())
}
