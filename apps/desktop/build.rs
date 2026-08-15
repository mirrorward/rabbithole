//! Build script: stamp the *workspace* version into the desktop app.
//!
//! `apps/desktop` is its own cargo workspace (it has to be — Tauri's bundler
//! and wry/webkit must not join `cargo build --workspace` at the repo root),
//! so it can't inherit `version.workspace = true`. Keep this crate's
//! `version` and `tauri.conf.json` equal to the product workspace version
//! anyway; About also stamps `RH_VERSION` from the root manifest at build
//! time so a drift cannot hide in the panel. A git short SHA is appended on
//! debug builds.

use std::process::Command;

fn main() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    println!("cargo:rerun-if-changed={root}");

    let version = std::fs::read_to_string(root)
        .ok()
        .and_then(|s| {
            // The workspace `[workspace.package] version = "x.y.z"` line.
            s.lines()
                .find(|l| l.trim_start().starts_with("version = \""))
                .and_then(|l| l.split('"').nth(1).map(str::to_string))
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=RH_VERSION={version}");

    // A dev build says so, and says which commit — otherwise "0.185.0" from a
    // working tree with uncommitted changes is a claim it can't support.
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=RH_GIT_SHA={sha}");

    tauri_build::build()
}
