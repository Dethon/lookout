//! On Windows, links the SQLite that comes with the system.
//!
//! rusqlite links a library named `sqlite3`; this writes an import library
//! by that name for `winsqlite3.dll`, so SQLite needn't be compiled from C
//! and no C toolchain is needed to cross-compile.

use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=winsqlite3.def");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let machine = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => implib::MachineType::ARM64,
        Ok("x86") => implib::MachineType::I386,
        _ => implib::MachineType::AMD64,
    };
    let def = std::fs::read_to_string("winsqlite3.def").expect("winsqlite3.def is readable");
    let library = implib::ImportLibrary::new(&def, machine, implib::Flavor::Msvc).expect("winsqlite3.def parses");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut file = std::fs::File::create(out_dir.join("sqlite3.lib")).expect("OUT_DIR is writable");
    library.write_to(&mut file).expect("the import library is written");
    println!("cargo::rustc-link-search=native={}", out_dir.display());
}
