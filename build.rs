//! Compiles the vendored Daslang tree-sitter grammar, but only when the
//! `daslang` feature is enabled. The grammar ships no Rust crate, so its
//! generated `parser.c` (~12 MB) and external `scanner.c` live under
//! `vendor/tree-sitter-daslang/` and are built here into a static lib that
//! exposes `tree_sitter_daslang()` (see src/lang.rs for the binding).
//!
//! Without the feature this is a no-op, so the default build pulls in neither
//! `cc` work nor the grammar.
//!
//! Feature detection uses `CARGO_FEATURE_DASLANG` (not `#[cfg(feature)]`) so the
//! build script always links the grammar when `cargo test --features daslang`
//! runs — cfg on build.rs has historically no-op'd in CI and left
//! `tree_sitter_daslang` undefined at link time.

fn main() {
    // Set by Cargo for each activated package feature (UPPER_SNAKE).
    if std::env::var_os("CARGO_FEATURE_DASLANG").is_some() {
        compile_daslang();
    }
}

fn compile_daslang() {
    use std::path::PathBuf;

    let dir: PathBuf = ["vendor", "tree-sitter-daslang"].iter().collect();
    let parser = dir.join("parser.c");
    let scanner = dir.join("scanner.c");

    if !parser.is_file() {
        panic!(
            "daslang feature enabled but {} is missing — vendor the grammar under vendor/tree-sitter-daslang/",
            parser.display()
        );
    }

    println!("cargo:rerun-if-changed={}", parser.display());
    println!("cargo:rerun-if-changed={}", scanner.display());
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("tree_sitter/parser.h").display()
    );

    cc::Build::new()
        .include(&dir) // resolves `#include "tree_sitter/parser.h"`
        .file(&parser)
        .file(&scanner)
        .warnings(false) // generated code is noisy
        // parser.c is one huge jump table + static data; -O1 keeps the ~12 MB
        // file compiling quickly without hurting parse speed.
        .opt_level(1)
        .compile("tree_sitter_daslang");
}
