// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Shipped binaries and wheels must never enable request-field failpoints.
//!
//! A workspace test build always unifies `catabolic-cli`'s dev-dependency
//! feature, so a default-feature build cannot run inside it. The refusal path
//! itself is covered end to end by the store unit test
//! `default_feature_builds_refuse_failpoint_requests_before_effects`. This
//! test checks the configuration that decides what ships.
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_owned()
}

/// `(section, line)` pairs of a TOML file, comments removed.
fn sections(text: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut lines = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap().trim();
        if line.starts_with('[') {
            section = line.to_owned();
        } else if !line.is_empty() {
            lines.push((section.clone(), line.to_owned()));
        }
    }
    lines
}

#[test]
fn only_dev_dependencies_enable_the_failpoints_feature() {
    let root = workspace();
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if manifest.exists() {
            manifests.push(manifest);
        }
    }
    assert!(manifests.len() >= 9, "{manifests:?}");
    let mut enabling = Vec::new();
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest).unwrap();
        for (section, line) in sections(&text) {
            if !line.contains("failpoints") {
                continue;
            }
            let store = manifest.ends_with("catabolic-store/Cargo.toml");
            if store && section == "[features]" && line.starts_with("failpoints") {
                continue;
            }
            if section == "[dev-dependencies]" {
                enabling.push(manifest.display().to_string());
                continue;
            }
            panic!(
                "{}: {section} enables failpoints outside dev-dependencies: {line}",
                manifest.display()
            );
        }
        // No default feature set may include it.
        for (section, line) in sections(&text) {
            if section == "[features]" && line.starts_with("default") {
                assert!(!line.contains("failpoints"), "{}", manifest.display());
            }
        }
    }
    assert_eq!(
        enabling,
        [root
            .join("crates/catabolic-cli/Cargo.toml")
            .display()
            .to_string()]
    );
}

#[test]
fn shipped_build_commands_do_not_select_features() {
    let root = workspace();
    let pyproject =
        std::fs::read_to_string(root.join("crates/catabolic-python/pyproject.toml")).unwrap();
    let maturin: Vec<_> = sections(&pyproject)
        .into_iter()
        .filter(|(section, line)| section == "[tool.maturin]" && line.starts_with("features"))
        .collect();
    assert_eq!(maturin.len(), 1, "{maturin:?}");
    assert!(!maturin[0].1.contains("failpoints"), "{maturin:?}");
    // The standalone preparation tool and the wheel builder both build
    // release artifacts with default features only.
    for tool in [
        "crates/catabolic-dist/src/main.rs",
        "scripts/build_native_wheel.py",
    ] {
        let text = std::fs::read_to_string(root.join(tool)).unwrap();
        assert!(text.contains("\"--release\""), "{tool}");
        for flag in ["\"--features\"", "\"--all-features\"", "failpoints"] {
            assert!(!text.contains(flag), "{tool} selects {flag}");
        }
    }
}
