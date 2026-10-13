// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Five exact, locally verified upstream inputs omitted from their registry packages.
//! This is an additive notice source, not a license resolver or legal audit.
use super::{Result, copy_new, digest, fail, identity};
use serde_json::{Value, json};
use std::{fs, path::Path};

const RESOURCES: &str = "crates/catabolic-dist/resources/supplemental-notices";
const PROVENANCE: &str = "licenses/supplemental-notices/manifest.json";
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
const MANIFEST: &[u8] = include_bytes!("../resources/supplemental-notices/manifest.json");

struct Notice {
    name: &'static str,
    version: &'static str,
    filename: &'static str,
    bytes: &'static [u8],
}

// Deliberately fixed at compile time: no user config, downloader, license
// templates, matching by license expression, or fallback for other versions.
macro_rules! notice {
    ($name:literal, $version:literal) => {
        Notice {
            name: $name,
            version: $version,
            filename: concat!($name, "-", $version, ".LICENSE.txt"),
            bytes: include_bytes!(concat!(
                "../resources/supplemental-notices/",
                $name,
                "-",
                $version,
                ".LICENSE.txt"
            )),
        }
    };
}
const NOTICES: &[Notice] = &[
    notice!("Inflector", "0.11.4"),
    notice!("jsonschema-regex", "0.58.6"),
    notice!("jsonschema-value", "0.58.6"),
    notice!("uuid-simd", "0.8.0"),
    notice!("vsimd", "0.8.0"),
];

impl Notice {
    fn package_id(&self) -> String {
        format!("{REGISTRY}#{}@{}", self.name, self.version)
    }

    fn target(&self) -> String {
        format!(
            "licenses/dependencies/{}-{}/UPSTREAM-LICENSE.txt",
            self.name, self.version
        )
    }

    fn receipt(&self) -> Value {
        json!({"notice_file":self.target(), "provenance_file":PROVENANCE})
    }

    fn evidence(&self) -> Result<Value> {
        let manifest: Value = serde_json::from_slice(MANIFEST)?;
        let packages = manifest["packages"]
            .as_array()
            .ok_or("notice evidence missing")?;
        let matches: Vec<_> = packages
            .iter()
            .filter(|p| p["package"] == self.name)
            .collect();
        if manifest["schema_version"] != 1 || packages.len() != NOTICES.len() || matches.len() != 1
        {
            return fail("supplemental notice catalog identity mismatch");
        }
        let evidence = matches[0];
        if evidence["version"] != self.version
            || evidence["package_id"] != self.package_id()
            || evidence["notice_file"] != self.filename
            || evidence["notice_sha256"] != digest(self.bytes)
            || evidence["notice_bytes"] != self.bytes.len()
            || evidence["upstream_revision"] != evidence["version_revision_evidence"]["git_sha1"]
            || evidence["version_revision_evidence"]["upstream_manifest_matches_cached_original_byte_for_byte"]
                != true
            || evidence["unresolved_ambiguities"] != json!([])
        {
            return fail(format!(
                "supplemental notice evidence mismatch: {}",
                self.name
            ));
        }
        Ok(evidence.clone())
    }
}

fn select(name: &str) -> Option<&'static Notice> {
    NOTICES.iter().find(|notice| notice.name == name)
}

fn check_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let actual = identity(path)?;
    if actual.sha256 != digest(bytes) || actual.bytes != bytes.len() as u64 || actual.executable {
        return fail(format!(
            "supplemental notice input changed: {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn check_source(root: &Path) -> Result<()> {
    check_bytes(
        &root.join("crates/catabolic-dist/src/supplemental.rs"),
        include_bytes!("supplemental.rs"),
    )?;
    check_bytes(&root.join(RESOURCES).join("manifest.json"), MANIFEST)?;
    for notice in NOTICES {
        notice.evidence()?;
        check_bytes(&root.join(RESOURCES).join(notice.filename), notice.bytes)?;
    }
    Ok(())
}

fn validate_metadata(notice: &Notice, evidence: &Value, package: &Value) -> Result<()> {
    if package["name"] != notice.name
        || package["version"] != notice.version
        || package["id"] != notice.package_id()
        || package["source"] != REGISTRY
        || package["license"] != evidence["license_expression"]
        || package["repository"] != evidence["upstream_repository"]
    {
        return fail(format!(
            "supplemental package identity mismatch: {}",
            notice.name
        ));
    }
    Ok(())
}

fn validate_cache(directory: &Path, evidence: &Value) -> Result<()> {
    let proof = &evidence["version_revision_evidence"];
    for (file, field) in [
        ("Cargo.toml", "cached_cargo_toml_sha256"),
        ("Cargo.toml.orig", "cached_cargo_toml_orig_sha256"),
        (".cargo_vcs_info.json", "cached_cargo_vcs_info_sha256"),
    ] {
        if identity(&directory.join(file))?.sha256
            != proof[field]
                .as_str()
                .ok_or("cached package digest missing")?
        {
            return fail(format!("supplemental package provenance changed: {file}"));
        }
    }
    let vcs: Value = serde_json::from_slice(&fs::read(directory.join(".cargo_vcs_info.json"))?)?;
    if vcs["git"]["sha1"] != evidence["upstream_revision"]
        || vcs["path_in_vcs"].as_str().unwrap_or("")
            != proof["path_in_vcs"]
                .as_str()
                .ok_or("VCS path evidence missing")?
    {
        return fail("supplemental package revision/path mismatch");
    }
    if let Some(expected) = proof["cached_readme_sha256"].as_str()
        && identity(&directory.join("README.md"))?.sha256 != expected
    {
        return fail("supplemental package README provenance changed");
    }
    Ok(())
}

pub(super) fn stage(root: &Path, out: &Path, package: &Value) -> Result<Option<Value>> {
    let Some(notice) = package["name"].as_str().and_then(select) else {
        return Ok(None);
    };
    let evidence = notice.evidence()?;
    validate_metadata(notice, &evidence, package)?;
    let manifest = Path::new(
        package["manifest_path"]
            .as_str()
            .ok_or("package manifest missing")?,
    );
    if manifest.file_name().is_none_or(|name| name != "Cargo.toml") {
        return fail("unexpected supplemental package manifest filename");
    }
    validate_cache(
        manifest.parent().ok_or("package directory missing")?,
        &evidence,
    )?;
    let resources = root.join(RESOURCES);
    check_bytes(&resources.join(notice.filename), notice.bytes)?;
    check_bytes(&resources.join("manifest.json"), MANIFEST)?;
    copy_new(&resources.join(notice.filename), &out.join(notice.target()))?;
    if out.join(PROVENANCE).exists() {
        check_bytes(&out.join(PROVENANCE), MANIFEST)?;
    } else {
        copy_new(&resources.join("manifest.json"), &out.join(PROVENANCE))?;
    }
    Ok(Some(notice.receipt()))
}

pub(super) fn verify(candidate: &Path) -> Result<()> {
    let records: Value =
        serde_json::from_slice(&fs::read(candidate.join("licenses/dependencies.json"))?)?;
    let mut used = std::collections::BTreeSet::new();
    for record in records
        .as_array()
        .ok_or("dependency notice inventory must be an array")?
    {
        let Some(notice) = record["package"].as_str().and_then(select) else {
            if record.get("supplemental_notice").is_some() {
                return fail("unsupported supplemental notice claim");
            }
            continue;
        };
        let evidence = notice.evidence()?;
        if record["version"] != notice.version
            || record["package_id"] != notice.package_id()
            || record["license_expression"] != evidence["license_expression"]
            || record["project_license"] != false
            || record["supplemental_notice"] != notice.receipt()
            || !used.insert(notice.name)
            || record["notice_files"].as_array().is_none_or(|files| {
                files
                    .iter()
                    .filter(|file| **file == notice.target())
                    .count()
                    != 1
            })
        {
            return fail(format!(
                "supplemental notice receipt mismatch: {}",
                notice.name
            ));
        }
        check_bytes(&candidate.join(notice.target()), notice.bytes)?;
    }
    if !used.is_empty() {
        check_bytes(&candidate.join(PROVENANCE), MANIFEST)?;
    } else if candidate.join(PROVENANCE).exists() {
        return fail("supplemental provenance without a covered package");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{json_new, write_new};

    fn resources(root: &Path) {
        write_new(
            &root.join("crates/catabolic-dist/src/supplemental.rs"),
            include_bytes!("supplemental.rs"),
        )
        .unwrap();
        write_new(&root.join(RESOURCES).join("manifest.json"), MANIFEST).unwrap();
        for notice in NOTICES {
            write_new(&root.join(RESOURCES).join(notice.filename), notice.bytes).unwrap();
        }
    }

    fn record(notice: &Notice) -> Value {
        json!({"package":notice.name,"version":notice.version,"package_id":notice.package_id(),
            "license_expression":notice.evidence().unwrap()["license_expression"],
            "project_license":false,"notice_files":[notice.target()],"supplemental_notice":notice.receipt()})
    }

    fn candidate(out: &Path) -> Vec<Value> {
        write_new(&out.join(PROVENANCE), MANIFEST).unwrap();
        let records: Vec<_> = NOTICES.iter().map(record).collect();
        for notice in NOTICES {
            write_new(&out.join(notice.target()), notice.bytes).unwrap();
        }
        json_new(&out.join("licenses/dependencies.json"), &records).unwrap();
        records
    }

    #[test]
    fn five_embedded_notices_and_source_provenance_are_exact() {
        let tmp = tempfile::tempdir().unwrap();
        resources(tmp.path());
        check_source(tmp.path()).unwrap();
        for notice in NOTICES {
            let path = tmp.path().join(RESOURCES).join(notice.filename);
            fs::write(&path, b"changed copyright text").unwrap();
            assert!(check_source(tmp.path()).is_err());
            fs::write(path, notice.bytes).unwrap();
        }
        for (path, original) in [
            (tmp.path().join(RESOURCES).join("manifest.json"), MANIFEST),
            (
                tmp.path().join("crates/catabolic-dist/src/supplemental.rs"),
                include_bytes!("supplemental.rs").as_slice(),
            ),
        ] {
            fs::write(&path, b"changed provenance or preparer").unwrap();
            assert!(check_source(tmp.path()).is_err());
            fs::write(path, original).unwrap();
        }
    }

    #[test]
    fn supplemental_metadata_cannot_match_other_versions_or_provenance() {
        for notice in NOTICES {
            let evidence = notice.evidence().unwrap();
            let package = json!({"name":notice.name,"version":notice.version,"id":notice.package_id(),
                "source":REGISTRY,"license":evidence["license_expression"],"repository":evidence["upstream_repository"]});
            validate_metadata(notice, &evidence, &package).unwrap();
            for field in ["name", "version", "id", "source", "license", "repository"] {
                let mut changed = package.clone();
                changed[field] = json!("wrong");
                assert!(
                    validate_metadata(notice, &evidence, &changed).is_err(),
                    "{field}"
                );
            }
        }
        assert!(
            stage(
                Path::new("unused"),
                Path::new("unused"),
                &json!({"name":"other-mit-package"})
            )
            .unwrap()
            .is_none()
        );
    }

    fn cache_fixture(directory: &Path) -> Value {
        let mut evidence = NOTICES[0].evidence().unwrap();
        let vcs = json!({"git":{"sha1":evidence["upstream_revision"]},"path_in_vcs":""});
        for (file, field, bytes) in [
            (
                "Cargo.toml",
                "cached_cargo_toml_sha256",
                b"normalized package fixture".to_vec(),
            ),
            (
                "Cargo.toml.orig",
                "cached_cargo_toml_orig_sha256",
                b"upstream package fixture".to_vec(),
            ),
            (
                ".cargo_vcs_info.json",
                "cached_cargo_vcs_info_sha256",
                serde_json::to_vec(&vcs).unwrap(),
            ),
            (
                "README.md",
                "cached_readme_sha256",
                b"fixture license reference".to_vec(),
            ),
        ] {
            write_new(&directory.join(file), &bytes).unwrap();
            evidence["version_revision_evidence"][field] = json!(digest(&bytes));
        }
        evidence
    }

    #[test]
    fn cached_package_digests_revision_and_vcs_path_are_required() {
        let tmp = tempfile::tempdir().unwrap();
        let evidence = cache_fixture(tmp.path());
        validate_cache(tmp.path(), &evidence).unwrap();
        for file in [
            "Cargo.toml",
            "Cargo.toml.orig",
            ".cargo_vcs_info.json",
            "README.md",
        ] {
            let path = tmp.path().join(file);
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"drift").unwrap();
            assert!(validate_cache(tmp.path(), &evidence).is_err(), "{file}");
            fs::remove_file(&path).unwrap();
            assert!(validate_cache(tmp.path(), &evidence).is_err(), "{file}");
            fs::write(&path, original).unwrap();
        }
        let mut wrong_revision = evidence.clone();
        wrong_revision["upstream_revision"] = json!("wrong revision");
        assert!(validate_cache(tmp.path(), &wrong_revision).is_err());
        let mut wrong_path = evidence.clone();
        wrong_path["version_revision_evidence"]["path_in_vcs"] = json!("other/package");
        assert!(validate_cache(tmp.path(), &wrong_path).is_err());
    }

    #[test]
    fn candidate_notices_require_exact_bytes_and_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        let records = candidate(tmp.path());
        verify(tmp.path()).unwrap();
        for notice in NOTICES {
            let path = tmp.path().join(notice.target());
            fs::write(&path, b"wrong notice").unwrap();
            assert!(verify(tmp.path()).is_err());
            fs::remove_file(&path).unwrap();
            assert!(verify(tmp.path()).is_err());
            fs::write(path, notice.bytes).unwrap();
        }
        let inventory = tmp.path().join("licenses/dependencies.json");
        for field in [
            "version",
            "package_id",
            "license_expression",
            "project_license",
            "notice_files",
            "supplemental_notice",
        ] {
            let mut changed = records.clone();
            changed[0][field] = json!("wrong");
            fs::write(&inventory, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(verify(tmp.path()).is_err(), "{field}");
        }
        let mut missing = records.clone();
        missing[0]
            .as_object_mut()
            .unwrap()
            .remove("supplemental_notice");
        fs::write(&inventory, serde_json::to_vec(&missing).unwrap()).unwrap();
        assert!(verify(tmp.path()).is_err());
        let mut duplicate = records.clone();
        duplicate.push(records[0].clone());
        fs::write(&inventory, serde_json::to_vec(&duplicate).unwrap()).unwrap();
        assert!(verify(tmp.path()).is_err());
        let mut unknown = records.clone();
        unknown[0]["package"] = json!("unknown-package");
        fs::write(&inventory, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(verify(tmp.path()).is_err());
        fs::write(&inventory, serde_json::to_vec(&records).unwrap()).unwrap();
        fs::write(tmp.path().join(PROVENANCE), b"{}").unwrap();
        assert!(verify(tmp.path()).is_err());
        fs::remove_file(tmp.path().join(PROVENANCE)).unwrap();
        assert!(verify(tmp.path()).is_err());
    }

    #[test]
    fn anchored_candidate_verification_checks_supplemental_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        crate::tests::fixture(&tmp.path().join("source"), &out);
        fs::remove_file(out.join("licenses/dependencies.json")).unwrap();
        candidate(&out);
        let reanchor = || {
            let mut manifest: crate::Candidate =
                serde_json::from_slice(&fs::read(out.join(crate::MANIFEST)).unwrap()).unwrap();
            manifest.payload.clear();
            crate::collect(&out, &out, &mut manifest.payload).unwrap();
            manifest.payload.remove(crate::MANIFEST);
            fs::write(
                out.join(crate::MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            identity(&out.join(crate::MANIFEST)).unwrap().sha256
        };
        let hash = reanchor();
        crate::verify(&out, &hash, None).unwrap();
        fs::write(out.join(NOTICES[0].target()), b"changed license text").unwrap();
        assert!(crate::verify(&out, &hash, None).is_err());
        // Even a fresh manifest anchor cannot assert coverage for a wrong text.
        let hash = reanchor();
        assert!(
            crate::verify(&out, &hash, None)
                .unwrap_err()
                .to_string()
                .contains("supplemental notice input changed")
        );
    }

    #[test]
    fn ordinary_registry_notice_discovery_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let out = tmp.path().join("out");
        let package = tmp.path().join("ordinary");
        write_new(&root.join("LICENSE"), b"project license").unwrap();
        write_new(&package.join("LICENSE"), b"ordinary license").unwrap();
        let metadata = json!({"packages":[{"name":"ordinary","version":"1.0.0","id":"ordinary-id", "source":REGISTRY,"license":"MIT","manifest_path":package.join("Cargo.toml")} ]});
        crate::stage_licenses(
            &root,
            &out,
            &metadata,
            &std::collections::BTreeSet::from(["ordinary-id".to_string()]),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            fs::read(out.join("licenses/dependencies/ordinary-1.0.0/LICENSE")).unwrap(),
            b"ordinary license"
        );
        assert!(!out.join(PROVENANCE).exists());
        verify(&out).unwrap();
    }

    #[test]
    #[ignore = "requires the existing exact-version Cargo cache; never downloads or builds engine binaries"]
    fn stage_five_notices_from_existing_offline_cache() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        check_source(root).unwrap();
        let metadata: Value = serde_json::from_slice(
            &crate::checked(
                std::process::Command::new("cargo")
                    .args(["metadata", "--offline", "--locked", "--format-version", "1"])
                    .current_dir(root),
                60,
                16 * 1024 * 1024,
            )
            .unwrap(),
        )
        .unwrap();
        let packages = NOTICES.iter().map(Notice::package_id).collect();
        let tmp = tempfile::tempdir().unwrap();
        crate::stage_licenses(
            root,
            tmp.path(),
            &metadata,
            &packages,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        verify(tmp.path()).unwrap();
        let records: Vec<Value> = serde_json::from_slice(
            &fs::read(tmp.path().join("licenses/dependencies.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(records.len(), 5);
        assert!(
            records
                .iter()
                .all(|r| r["supplemental_notice"]["provenance_file"] == PROVENANCE)
        );
        for notice in NOTICES {
            assert_eq!(
                fs::read(tmp.path().join(notice.target())).unwrap(),
                notice.bytes
            );
        }
    }
}
