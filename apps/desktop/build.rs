//! Build script: stamp the *workspace* version into the desktop app.
//!
//! `apps/desktop` is its own cargo workspace (it has to be — Tauri's bundler
//! and the wasm SPA can't share one), so it can't inherit
//! `version.workspace = true`. Its own `version` therefore drifts: the About
//! panel was reporting 0.105.0 while the app it shipped was 0.185.0.
//!
//! So the version comes from the root workspace manifest at build time, plus a
//! git short SHA on debug builds — "0.185.0 (dev 1a2b3c4)" tells you exactly
//! what you're running, which is the entire point of a version in an About box.

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
