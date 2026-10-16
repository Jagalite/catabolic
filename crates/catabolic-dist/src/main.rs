// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Local, unqualified native candidate preparation. No publishing or Python packaging.
mod supplemental;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 8192;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
const MANIFEST: &str = "candidate.json";
const STATUS: &str = "unqualified-local-native-candidate";
const NATIVE_CRATES: &[&str] = &[
    "catabolic-core",
    "catabolic-store",
    "catabolic-runtime",
    "catabolic-http",
    "catabolic-cli",
    "catabolic-dist",
];
const LIMITS: &str = "This is a local native executable candidate, not a qualified release. No M4-M11, M6 output-safety, HTTP, platform, complete CLI parity, dependency-license audit, or reproducible-build gate is closed. Native Rust libraries remain available through their crates; no stable C ABI is provided. Python packaging and src/catabolic business implementation are not staged. The optional notification provider path requires separate current-source qualification. Source/payload checks assume trusted, non-concurrently modified directories; they are not hostile-directory or executable-attestation protection. Binaries are dynamically linked for the recorded host; portability is not established.\n";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    sha256: String,
    bytes: u64,
    executable: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    format: u32,
    status: String,
    version: String,
    source_sha256: String,
    source_files: BTreeMap<String, FileIdentity>,
    source_checkout: PathBuf,
    git_head: String,
    rustc: String,
    cargo: String,
    build_environment_sha256: String,
    cargo_config_sha256: BTreeMap<String, String>,
    preparation_tool_sha256: String,
    build_command: Vec<String>,
    binaries: BTreeMap<String, String>,
    payload: BTreeMap<String, FileIdentity>,
    python_free_runtime_accepted: bool,
    apprise_helper_source_present: bool,
    qualification: String,
}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(message.into().into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn relative(path: &Path) -> Result<String> {
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return fail("expected a normal relative path");
    }
    Ok(path.to_str().ok_or("non-UTF-8 path")?.replace('\\', "/"))
}
fn identity(path: &Path) -> Result<FileIdentity> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_FILE {
        return fail(format!("not a bounded regular file: {}", path.display()));
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = path.extension().is_some_and(|x| x == "exe");
    let bytes = fs::read(path)?;
    if bytes.len() as u64 != m.len() {
        return fail("file changed while reading");
    }
    Ok(FileIdentity {
        sha256: digest(&bytes),
        bytes: m.len(),
        executable,
    })
}
fn collect(root: &Path, at: &Path, out: &mut BTreeMap<String, FileIdentity>) -> Result<()> {
    let m = fs::symlink_metadata(at)?;
    if m.file_type().is_symlink() {
        return fail("symlinks are not candidate/source inputs");
    }
    if m.is_dir() {
        for entry in fs::read_dir(at)? {
            collect(root, &entry?.path(), out)?;
        }
    } else {
        out.insert(relative(at.strip_prefix(root)?)?, identity(at)?);
        if out.len() > MAX_FILES || out.values().map(|x| x.bytes).sum::<u64>() > MAX_TOTAL {
            return fail("inventory exceeds preparation budget");
        }
    }
    Ok(())
}
fn source_snapshot(root: &Path) -> Result<BTreeMap<String, FileIdentity>> {
    let mut files = BTreeMap::new();
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "LICENSE"] {
        collect(root, &root.join(name), &mut files)?;
    }
    for name in NATIVE_CRATES {
        collect(root, &root.join("crates").join(name), &mut files)?;
    }
    if root.join(".cargo").exists() {
        collect(root, &root.join(".cargo"), &mut files)?;
    }
    Ok(files)
}
fn source_hash(files: &BTreeMap<String, FileIdentity>) -> Result<String> {
    Ok(digest(&serde_json::to_vec(files)?))
}
fn cargo_home_path(root: &Path, home: PathBuf) -> PathBuf {
    if home.is_absolute() {
        home
    } else {
        root.join(home)
    }
}
fn cargo_home_setting(cargo: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = cargo {
        if path.as_os_str().is_empty() {
            return fail("CARGO_HOME must not be empty");
        }
        return Ok(path);
    }
    let home = home
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or("set CARGO_HOME or HOME explicitly for configuration provenance")?;
    Ok(home.join(".cargo"))
}
fn config_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut directories: BTreeSet<PathBuf> = root.ancestors().map(|p| p.join(".cargo")).collect();
    let home = cargo_home_setting(
        env::var_os("CARGO_HOME").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
    )?;
    // Child Cargo runs from root, even when the preparer was launched elsewhere.
    directories.insert(cargo_home_path(root, home));
    for directory in directories {
        for name in ["config", "config.toml"] {
            let p = directory.join(name);
            if p.exists() {
                result.insert(p.to_string_lossy().into_owned(), identity(&p)?.sha256);
            }
        }
    }
    Ok(result)
}
fn recorded_environment_name(key: &str) -> bool {
    if [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "KEY",
        "PRIVATE",
        "PWD",
    ]
    .iter()
    .any(|part| key.to_ascii_uppercase().contains(part))
    {
        return false;
    }
    const EXACT: &[&str] = &[
        "PATH",
        "HOME",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "CARGO_INCREMENTAL",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "RUSTC",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "CC",
        "CXX",
        "AR",
        "RANLIB",
        "CFLAGS",
        "CXXFLAGS",
        "CPPFLAGS",
        "ARFLAGS",
        "LDFLAGS",
        "LIBSQLITE3_FLAGS",
        "MACOSX_DEPLOYMENT_TARGET",
        "SDKROOT",
        "CARGO_BUILD_JOBS",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTDOC",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTDOCFLAGS",
        "CARGO_BUILD_INCREMENTAL",
        "CARGO_BUILD_DEP_INFO_BASEDIR",
    ];
    EXACT.contains(&key)
        || (key.starts_with("CARGO_PROFILE_")
            && [
                "_OPT_LEVEL",
                "_DEBUG",
                "_SPLIT_DEBUGINFO",
                "_STRIP",
                "_DEBUG_ASSERTIONS",
                "_OVERFLOW_CHECKS",
                "_LTO",
                "_PANIC",
                "_INCREMENTAL",
                "_CODEGEN_UNITS",
                "_RPATH",
            ]
            .iter()
            .any(|suffix| key.ends_with(suffix)))
        || (key.starts_with("CARGO_TARGET_")
            && (key.ends_with("_LINKER") || key.ends_with("_RUSTFLAGS")))
        || [
            "CC_",
            "CXX_",
            "AR_",
            "RANLIB_",
            "CFLAGS_",
            "CXXFLAGS_",
            "CPPFLAGS_",
            "ARFLAGS_",
        ]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix).is_some_and(|target| {
                target
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                    && target.split(['-', '_']).count() >= 3
            })
        })
        || key
            .strip_prefix("HOST_")
            .or_else(|| key.strip_prefix("TARGET_"))
            .is_some_and(|name| {
                [
                    "CC", "CXX", "AR", "RANLIB", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "ARFLAGS",
                ]
                .contains(&name)
            })
}
fn environment_hash() -> Result<String> {
    // Deliberately narrow compiler/build inputs only. Credentials, registry
    // tokens, proxy settings and arbitrary environment variables are excluded.
    // The values of accepted inputs are hashed, never written to the manifest.
    let mut vars: Vec<_> = env::vars_os()
        .filter(|(k, _)| k.to_str().is_some_and(recorded_environment_name))
        .map(|(k, v)| (k.as_encoded_bytes().to_vec(), v.as_encoded_bytes().to_vec()))
        .collect();
    vars.sort();
    Ok(digest(&serde_json::to_vec(&vars)?))
}
fn command_output(
    command: &mut Command,
    seconds: u64,
    max_bytes: u64,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code().unwrap_or(-1);
        }
        if start.elapsed() > Duration::from_secs(seconds)
            || stdout.metadata()?.len() > max_bytes
            || stderr.metadata()?.len() > max_bytes
        {
            #[cfg(unix)]
            // SAFETY: the child was placed in its own process group above; this
            // negative PID addresses only that group while its leader is live.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return fail("command exceeded time/output budget");
        }
        thread::sleep(Duration::from_millis(25));
    };
    use std::io::{Seek, SeekFrom};
    let mut a = stdout;
    let mut b = stderr;
    a.seek(SeekFrom::Start(0))?;
    b.seek(SeekFrom::Start(0))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    a.take(max_bytes + 1).read_to_end(&mut out)?;
    b.take(max_bytes + 1).read_to_end(&mut err)?;
    if out.len() as u64 > max_bytes || err.len() as u64 > max_bytes {
        return fail("command output exceeds budget");
    }
    Ok((code, out, err))
}
fn checked(command: &mut Command, seconds: u64, max_bytes: u64) -> Result<Vec<u8>> {
    let (code, out, err) = command_output(command, seconds, max_bytes)?;
    if code != 0 {
        return fail(format!(
            "command failed ({code}): {}",
            String::from_utf8_lossy(&err)
        ));
    }
    Ok(out)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn copy_new(source: &Path, dest: &Path) -> Result<()> {
    let before = identity(source)?;
    write_new(dest, &fs::read(source)?)?;
    fs::set_permissions(dest, fs::metadata(source)?.permissions())?;
    if identity(source)? != before || identity(dest)? != before {
        return fail("copied input changed");
    }
    Ok(())
}
fn json_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}
fn reserve_output(path: &Path) -> Result<()> {
    fs::create_dir(path)
        .map_err(|e| format!("output must be a new directory (no overwrite): {e}"))?;
    write_new(
        &path.join("INCOMPLETE"),
        b"Preparation did not finish. Do not use as success evidence.\n",
    )
}
fn artifact_paths(
    messages: &[u8],
    cli_id: &str,
    root: &Path,
) -> Result<(BTreeMap<String, PathBuf>, BTreeSet<String>)> {
    let mut paths = BTreeMap::new();
    let mut packages = BTreeSet::new();
    for line in messages.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let v: Value = serde_json::from_slice(line)?;
        if v["reason"] != "compiler-artifact" {
            continue;
        }
        packages.insert(
            v["package_id"]
                .as_str()
                .ok_or("missing artifact package ID")?
                .to_string(),
        );
        let Some(executable) = v["executable"].as_str() else {
            continue;
        };
        let name = v["target"]["name"]
            .as_str()
            .ok_or("missing artifact target")?;
        let expected = match name {
            "catabolic-native" => "src/main.rs",
            "catabolic-legacy-native" => "src/legacy_main.rs",
            _ => return fail("unexpected executable in CLI build"),
        };
        let src = root
            .join("crates/catabolic-cli")
            .join(expected)
            .canonicalize()?;
        if v["package_id"] != cli_id
            || v["target"]["kind"] != json!(["bin"])
            || Path::new(
                v["target"]["src_path"]
                    .as_str()
                    .ok_or("missing target source")?,
            )
            .canonicalize()?
                != src
        {
            return fail("Cargo executable target identity mismatch");
        }
        if paths
            .insert(name.to_string(), PathBuf::from(executable))
            .is_some()
        {
            return fail("duplicate executable artifact");
        }
    }
    if paths.len() != 2 {
        return fail("Cargo must identify exactly the two native CLI executables");
    }
    Ok((paths, packages))
}
fn license_files(root: &Path, at: &Path, output: &mut BTreeSet<PathBuf>) -> Result<()> {
    let m = fs::symlink_metadata(at)?;
    if m.file_type().is_symlink() {
        return fail("license input must not be a symlink");
    }
    if m.is_file() {
        identity(at)?;
        output.insert(at.strip_prefix(root)?.into());
    } else if m.is_dir() {
        for entry in fs::read_dir(at)? {
            license_files(root, &entry?.path(), output)?;
        }
    }
    Ok(())
}
fn stage_licenses(
    root: &Path,
    out: &Path,
    metadata: &Value,
    packages: &BTreeSet<String>,
    sources: &BTreeMap<String, FileIdentity>,
) -> Result<()> {
    copy_new(
        &root.join("LICENSE"),
        &out.join("licenses/Catabolic-MIT.txt"),
    )?;
    let mut records = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("missing metadata packages")?
    {
        if !packages.contains(package["id"].as_str().ok_or("package ID missing")?) {
            continue;
        }
        let name = package["name"].as_str().ok_or("package name missing")?;
        let version = package["version"].as_str().ok_or("version missing")?;
        let manifest_dir = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("manifest path missing")?,
        )
        .parent()
        .ok_or("manifest parent missing")?;
        let mut paths = BTreeSet::new();
        for entry in fs::read_dir(manifest_dir)? {
            let entry = entry?;
            let n = entry.file_name().to_string_lossy().to_ascii_uppercase();
            if ["LICENSE", "LICENCE", "COPYING", "NOTICE", "AUTHORS"]
                .iter()
                .any(|prefix| n.starts_with(prefix))
            {
                license_files(manifest_dir, &entry.path(), &mut paths)?;
            }
        }
        if let Some(path) = package["license_file"].as_str() {
            license_files(manifest_dir, &manifest_dir.join(path), &mut paths)?;
        }
        let mut staged = Vec::new();
        for path in paths {
            let rel = relative(&path)?;
            let target = format!("licenses/dependencies/{name}-{version}/{rel}");
            relative(Path::new(&target))?;
            copy_new(&manifest_dir.join(&path), &out.join(&target))?;
            staged.push(target);
        }
        let supplement = supplemental::stage(root, out, package)?;
        if let Some(notice) = &supplement {
            staged.push(
                notice["notice_file"]
                    .as_str()
                    .ok_or("supplemental notice path missing")?
                    .to_string(),
            );
        }
        let mut record = json!({"package":name,"version":version,"package_id":package["id"],"license_expression":package["license"],"notice_files":staged,"project_license":package["source"].is_null()});
        if let Some(notice) = supplement {
            record["supplemental_notice"] = notice;
        }
        records.push(record);
    }
    // Frozen resources retain their adjacent provenance/license notices.
    for path in sources.keys().filter(|p| p.ends_with(".license")) {
        copy_new(&root.join(path), &out.join("licenses/resources").join(path))?;
    }
    let contract_license =
        root.join("crates/catabolic-http/resources/releases/1.7.0/openapi.json.license");
    if contract_license.exists() {
        copy_new(
            &contract_license,
            &out.join("licenses/resources/openapi.json.license"),
        )?;
    }
    json_new(&out.join("licenses/dependencies.json"), &records)
}
fn readonly_tree(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            readonly_tree(&entry?.path())?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() || identity(path)?.executable {
            0o555
        } else {
            0o444
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
fn prepare(root: &Path, output: &Path) -> Result<String> {
    reserve_output(output)?;
    let result = prepare_reserved(root, output);
    if result.is_err() {
        // Only this invocation's newly reserved output is touched. Failed work
        // keeps an explicit incomplete marker and cannot retain success metadata.
        let _ = fs::remove_file(output.join(MANIFEST));
        if !output.join("INCOMPLETE").exists() {
            let _ = write_new(&output.join("INCOMPLETE"), b"Preparation failed.\n");
        }
    }
    result
}
fn prepare_reserved(root: &Path, output: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    if !cfg!(unix) {
        return fail("this candidate preparer currently supports Unix hosts only");
    }
    if identity(&root.join("crates/catabolic-dist/src/main.rs"))?.sha256
        != digest(include_bytes!("main.rs"))
        || identity(&root.join("crates/catabolic-dist/Cargo.toml"))?.sha256
            != digest(include_bytes!("../Cargo.toml"))
    {
        return fail("preparation tool source changed; rebuild catabolic-dist first");
    }
    supplemental::check_source(&root)?;
    let sources = source_snapshot(&root)?;
    let rustc = String::from_utf8(checked(
        Command::new("rustc").arg("-vV").current_dir(&root),
        30,
        65536,
    )?)?;
    let cargo = String::from_utf8(checked(
        Command::new("cargo").arg("--version").current_dir(&root),
        30,
        65536,
    )?)?;
    if !rustc.starts_with("rustc 1.99.0 ") {
        return fail("requires pinned Rust 1.99.0");
    }
    let configs = config_hashes(&root)?;
    let environment = environment_hash()?;
    let metadata: Value = serde_json::from_slice(&checked(
        Command::new("cargo")
            .args(["metadata", "--offline", "--locked", "--format-version", "1"])
            .current_dir(&root),
        60,
        16 * 1024 * 1024,
    )?)?;
    let cli = metadata["packages"]
        .as_array()
        .ok_or("packages missing")?
        .iter()
        .find(|p| p["name"] == "catabolic-cli" && p["source"].is_null())
        .ok_or("CLI workspace package missing")?;
    if cli["version"] != env!("CARGO_PKG_VERSION") {
        return fail("preparation tool and CLI package versions differ");
    }
    let command = [
        "build",
        "--offline",
        "--locked",
        "--release",
        "--jobs",
        "1",
        "-p",
        "catabolic-cli",
        "--bin",
        "catabolic-native",
        "--bin",
        "catabolic-legacy-native",
        "--message-format=json-render-diagnostics",
    ];
    let messages = checked(
        Command::new("cargo").args(command).current_dir(&root),
        1200,
        16 * 1024 * 1024,
    )?;
    let (binaries, packages) = artifact_paths(
        &messages,
        cli["id"].as_str().ok_or("CLI ID missing")?,
        &root,
    )?;
    if packages
        .iter()
        .any(|p| p.contains("catabolic-python") || p.contains("pyo3") || p.contains("maturin"))
    {
        return fail("Python build dependency unexpectedly used");
    }
    for (name, target) in [
        ("catabolic-legacy-native", "bin/catabolic"),
        ("catabolic-native", "bin/catabolic-native"),
    ] {
        if !identity(&binaries[name])?.executable {
            return fail("Cargo binary is not executable");
        }
        copy_new(&binaries[name], &output.join(target))?;
    }
    stage_licenses(&root, output, &metadata, &packages, &sources)?;
    write_new(&output.join("STATUS.txt"), LIMITS.as_bytes())?;
    if sources != source_snapshot(&root)?
        || configs != config_hashes(&root)?
        || environment != environment_hash()?
    {
        return fail("source or build context changed during preparation");
    }
    let mut payload = BTreeMap::new();
    collect(output, output, &mut payload)?;
    payload.remove("INCOMPLETE");
    let candidate = Candidate {
        format: 1,
        status: STATUS.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        source_sha256: source_hash(&sources)?,
        apprise_helper_source_present: sources
            .contains_key("crates/catabolic-runtime/src/apprise_worker.py"),
        source_files: sources,
        source_checkout: root.clone(),
        git_head: String::from_utf8(checked(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root),
            30,
            65536,
        )?)?
        .trim()
        .into(),
        rustc,
        cargo,
        build_environment_sha256: environment,
        cargo_config_sha256: configs,
        preparation_tool_sha256: identity(&env::current_exe()?)?.sha256,
        build_command: command.into_iter().map(str::to_string).collect(),
        binaries: BTreeMap::from([
            ("bin/catabolic".into(), "catabolic-legacy-native".into()),
            ("bin/catabolic-native".into(), "catabolic-native".into()),
        ]),
        payload,
        python_free_runtime_accepted: false,
        qualification: LIMITS.into(),
    };
    json_new(&output.join(MANIFEST), &candidate)?;
    fs::remove_file(output.join("INCOMPLETE"))?;
    let hash = identity(&output.join(MANIFEST))?.sha256;
    verify(output, &hash, Some(&root))?;
    readonly_tree(output)?;
    Ok(hash)
}
fn inventory_dirs(root: &Path, at: &Path, dirs: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(at)? {
        let p = entry?.path();
        let m = fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            return fail("candidate symlink is forbidden");
        }
        if m.is_dir() {
            dirs.insert(relative(p.strip_prefix(root)?)?);
            inventory_dirs(root, &p, dirs)?;
        }
    }
    Ok(())
}
fn verify(candidate: &Path, manifest_hash: &str, source: Option<&Path>) -> Result<Candidate> {
    if manifest_hash.len() != 64 || !manifest_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return fail("expected an external SHA-256 manifest anchor");
    }
    if identity(&candidate.join(MANIFEST))?.sha256 != manifest_hash {
        return fail("candidate manifest differs from external anchor");
    }
    let manifest: Candidate = serde_json::from_slice(&fs::read(candidate.join(MANIFEST))?)?;
    if manifest.format != 1
        || manifest.status != STATUS
        || manifest.python_free_runtime_accepted
        || manifest.qualification != LIMITS
    {
        return fail("unexpected candidate/qualification status");
    }
    let expected = BTreeMap::from([
        ("bin/catabolic".into(), "catabolic-legacy-native".into()),
        ("bin/catabolic-native".into(), "catabolic-native".into()),
    ]);
    if manifest.binaries != expected {
        return fail("unexpected binary roles");
    }
    for path in manifest.payload.keys() {
        relative(Path::new(path))?;
    }
    let mut actual = BTreeMap::new();
    collect(candidate, candidate, &mut actual)?;
    actual.remove(MANIFEST);
    if actual != manifest.payload {
        return fail("candidate payload inventory or bytes changed");
    }
    if actual
        .keys()
        .any(|p| p.ends_with(".py") || p.ends_with(".pyc") || p.starts_with("src/catabolic/"))
    {
        return fail("unexpected Python payload");
    }
    for name in manifest.binaries.keys() {
        if !actual.get(name).is_some_and(|x| x.executable) {
            return fail("missing executable");
        }
    }
    if !actual.contains_key("licenses/Catabolic-MIT.txt")
        || !actual.contains_key("licenses/dependencies.json")
        || !actual.contains_key("STATUS.txt")
    {
        return fail("required status/license payload missing");
    }
    supplemental::verify(candidate)?;
    let mut dirs = BTreeSet::new();
    inventory_dirs(candidate, candidate, &mut dirs)?;
    let mut allowed_dirs = BTreeSet::new();
    for path in actual.keys() {
        let mut p = Path::new(path).parent();
        while let Some(dir) = p.filter(|p| !p.as_os_str().is_empty()) {
            allowed_dirs.insert(relative(dir)?);
            p = dir.parent();
        }
    }
    if dirs != allowed_dirs {
        return fail("unexpected candidate directory");
    }
    if source_hash(&manifest.source_files)? != manifest.source_sha256 {
        return fail("source inventory digest mismatch");
    }
    if let Some(root) = source
        && source_snapshot(root)? != manifest.source_files
    {
        return fail("current source differs from candidate source");
    }
    Ok(manifest)
}
fn main() {
    if let Err(error) = run() {
        eprintln!("catabolic-dist: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [action, root, out] if action == "prepare" => {
            let hash = prepare(Path::new(root), Path::new(out))?;
            println!(
                "{}",
                json!({"status":STATUS,"candidate":out,"manifest_sha256":hash})
            );
        }
        [action, candidate, hash] if action == "verify" => {
            verify(Path::new(candidate), hash, None)?;
            println!("candidate integrity verified; qualification remains open");
        }
        [action, candidate, hash, root] if action == "verify" => {
            verify(Path::new(candidate), hash, Some(Path::new(root)))?;
            println!("candidate and source integrity verified; qualification remains open");
        }
        [action, candidate, hash, report] if action == "smoke" => {
            smoke(Path::new(candidate), hash, Path::new(report))?
        }
        _ => {
            return fail(
                "usage: catabolic-dist prepare ROOT NEW_DIRECTORY | verify CANDIDATE MANIFEST_SHA256 [ROOT] | smoke CANDIDATE MANIFEST_SHA256 NEW_REPORT",
            );
        }
    }
    Ok(())
}

fn smoke(candidate: &Path, hash: &str, report: &Path) -> Result<()> {
    if report.exists() {
        return fail("smoke report already exists; refusing stale success/overwrite");
    }
    let candidate = candidate.canonicalize()?;
    let manifest = verify(&candidate, hash, None)?;
    let scratch = tempfile::Builder::new()
        .prefix("catabolic-native-smoke-")
        .tempdir_in("/tmp")?;
    let scratch_path = scratch.path().canonicalize()?;
    if scratch_path.starts_with(&manifest.source_checkout) || scratch_path.starts_with(&candidate) {
        return fail("smoke requires a working directory outside the checkout and artifact");
    }
    let path = scratch.path().join("denied-path");
    fs::create_dir(&path)?;
    let marker = scratch.path().join("python-attempted");
    for name in [
        "python",
        "python3",
        "python3.11",
        "python3.14",
        "pypy",
        "pypy3",
    ] {
        let script = path.join(name);
        write_new(
            &script,
            b"#!/bin/sh\nprintf 'denied\\n' >> \"$CATABOLIC_PYTHON_DENIAL_MARKER\"\nexit 97\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(script, fs::Permissions::from_mode(0o555))?;
        }
    }
    let db = scratch.path().join("catalog.sqlite3");
    let db = db.to_str().ok_or("non-UTF8 scratch")?;
    let cases: Vec<(&str, &str, Vec<&str>, i32)> = vec![
        ("preserved-version", "catabolic", vec!["--version"], 0),
        ("preserved-help", "catabolic", vec!["--help"], 0),
        (
            "companion-version",
            "catabolic-native",
            vec!["--version"],
            0,
        ),
        (
            "preserved-init",
            "catabolic",
            vec!["--json", "--db", db, "init"],
            0,
        ),
        (
            "companion-reopen",
            "catabolic-native",
            vec!["--db", db, "db", "inspect"],
            0,
        ),
        (
            "preserved-reopen",
            "catabolic",
            vec!["--json", "--db", db, "db", "status"],
            0,
        ),
        (
            "companion-http-gate",
            "catabolic-native",
            vec!["--db", db, "api", "serve"],
            2,
        ),
        (
            "preserved-http-gate",
            "catabolic",
            vec!["--db", db, "api", "serve"],
            2,
        ),
    ];
    let mut results = Vec::new();
    let mut database_id = None;
    let mut schema = None;
    for (name, executable, args, expected_code) in cases {
        let mut command = Command::new(candidate.join("bin").join(executable));
        command
            .args(&args)
            .current_dir(scratch.path())
            .env_clear()
            .env("PATH", &path)
            .env("HOME", scratch.path())
            .env("TMPDIR", scratch.path())
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("CATABOLIC_PYTHON_DENIAL_MARKER", &marker)
            .env(
                "CATABOLIC_NATIVE_BINARY",
                candidate.join("bin/catabolic-native"),
            );
        let (code, out, err) = command_output(&mut command, 30, 1024 * 1024)?;
        if code != expected_code {
            return fail(format!(
                "smoke {name} expected exit {expected_code}, got {code}: {}",
                String::from_utf8_lossy(&err)
            ));
        }
        match name {
            "preserved-version"
                if out != format!("catabolic {}\n", manifest.version).as_bytes() =>
            {
                return fail("preserved CLI binary role/version mismatch");
            }
            "preserved-help" if !String::from_utf8_lossy(&out).starts_with("usage: catabolic ") => {
                return fail("preserved CLI help mismatch");
            }
            "companion-version" => {
                let v: Value = serde_json::from_slice(&out)?;
                if v["version"] != manifest.version || !v["schema"].is_number() {
                    return fail("companion binary role/version mismatch");
                }
                schema = v["schema"].as_u64();
            }
            "companion-http-gate" | "preserved-http-gate"
                if !String::from_utf8_lossy(&err).contains("unqualified") =>
            {
                return fail("HTTP gate did not provide explicit unqualified refusal");
            }
            "preserved-init" | "companion-reopen" | "preserved-reopen" => {
                let v: Value = serde_json::from_slice(&out)?;
                let id = v["database_id"]
                    .as_str()
                    .ok_or("database identity missing")?;
                if v["schema"].as_u64() != schema || id.is_empty() {
                    return fail("catalog schema/identity mismatch");
                }
                if name == "preserved-init" {
                    database_id = Some(id.to_string());
                } else if database_id.as_deref() != Some(id) {
                    return fail("catalog did not reopen the same database");
                }
            }
            _ => (),
        }
        results.push(json!({"case":name,"exit_code":code,"stdout_sha256":digest(&out),"stderr_sha256":digest(&err)}));
        if marker.exists() {
            return fail("smoke attempted a denied Python executable");
        }
        verify(&candidate, hash, None)?;
    }
    let value = json!({"format":1,"status":"passed-bounded-safe-smoke-only","candidate_manifest_sha256":hash,
        "candidate_source_sha256":manifest.source_sha256,"cases":results,"outside_checkout_cwd":scratch.path(),
        "python_path_denied":true,"python_attempts":0,"python_free_runtime_accepted":false,
        "scope":"Clean environment and PATH denial sentinels; no arbitrary absolute-interpreter denial claim. Notification providers, external tools, dangerous operations, complete CLI/runtime parity and release gates are not qualified.",
        "candidate_status":STATUS});
    // A fresh receipt is only created after all checks pass. Existing receipts are never reused.
    json_new(report, &value)?;
    println!("bounded safe smoke passed; full runtime and release qualification remain open");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_provenance_excludes_credentials_and_arbitrary_variables() {
        for name in [
            "CC_x86_64_unknown_linux_gnu",
            "CFLAGS_x86_64-unknown-linux-gnu",
            "LIBSQLITE3_FLAGS",
            "HOST_CC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        ] {
            assert!(recorded_environment_name(name), "{name}");
        }
        for name in [
            "CARGO_REGISTRY_TOKEN",
            "CARGO_REGISTRIES_PRIVATE_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "HTTP_PROXY",
            "PASSWORD",
            "ARBITRARY_BUILD_SECRET",
            "CARGO_BUILD_TOKEN",
            "CARGO_PROFILE_SECRET_DEBUG",
            "CC_AUTH_TOKEN",
            "CC_API_KEY",
            "CC_PRIVATE_KEY",
            "AR_ACCESS_KEY",
        ] {
            assert!(!recorded_environment_name(name), "{name}");
        }
    }
    #[test]
    fn relative_cargo_home_uses_build_working_directory() {
        assert_eq!(
            cargo_home_path(Path::new("/source"), PathBuf::from("cache/cargo")),
            PathBuf::from("/source/cache/cargo")
        );
        assert_eq!(
            cargo_home_path(Path::new("/source"), PathBuf::from("/home/cargo")),
            PathBuf::from("/home/cargo")
        );
    }
    #[test]
    fn cargo_home_fallback_requires_an_explicit_nonempty_setting() {
        assert_eq!(
            cargo_home_setting(None, Some(PathBuf::from("/home/test"))).unwrap(),
            PathBuf::from("/home/test/.cargo")
        );
        assert!(cargo_home_setting(None, None).is_err());
        assert!(
            cargo_home_setting(Some(PathBuf::new()), Some(PathBuf::from("/home/test"))).is_err()
        );
        assert!(cargo_home_setting(None, Some(PathBuf::new())).is_err());
    }
    fn sources(root: &Path) {
        for file in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "LICENSE"] {
            write_new(&root.join(file), b"source").unwrap();
        }
        for name in NATIVE_CRATES {
            write_new(
                &root.join("crates").join(name).join("src/main.rs"),
                b"native source",
            )
            .unwrap();
        }
    }
    pub(super) fn fixture(root: &Path, out: &Path) -> String {
        sources(root);
        fs::create_dir(out).unwrap();
        for (path, bytes) in [
            ("bin/catabolic", b"legacy".as_slice()),
            ("bin/catabolic-native", b"companion"),
            ("licenses/Catabolic-MIT.txt", b"MIT"),
            ("licenses/dependencies.json", b"[]"),
            ("STATUS.txt", LIMITS.as_bytes()),
        ] {
            write_new(&out.join(path), bytes).unwrap();
            #[cfg(unix)]
            if path.starts_with("bin/") {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(out.join(path), fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let source_files = source_snapshot(root).unwrap();
        let mut payload = BTreeMap::new();
        collect(out, out, &mut payload).unwrap();
        let m = Candidate {
            format: 1,
            status: STATUS.into(),
            version: "0.2.0".into(),
            source_sha256: source_hash(&source_files).unwrap(),
            source_files,
            source_checkout: root.to_path_buf(),
            git_head: "fixture".into(),
            rustc: "fixture".into(),
            cargo: "fixture".into(),
            build_environment_sha256: "fixture".into(),
            cargo_config_sha256: BTreeMap::new(),
            preparation_tool_sha256: "fixture".into(),
            build_command: vec![],
            binaries: BTreeMap::from([
                ("bin/catabolic".into(), "catabolic-legacy-native".into()),
                ("bin/catabolic-native".into(), "catabolic-native".into()),
            ]),
            payload,
            python_free_runtime_accepted: false,
            apprise_helper_source_present: false,
            qualification: LIMITS.into(),
        };
        json_new(&out.join(MANIFEST), &m).unwrap();
        identity(&out.join(MANIFEST)).unwrap().sha256
    }
    #[test]
    fn source_change_and_added_source_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let out = tmp.path().join("out");
        let hash = fixture(&root, &out);
        verify(&out, &hash, Some(&root)).unwrap();
        fs::write(root.join("Cargo.lock"), b"changed").unwrap();
        assert!(verify(&out, &hash, Some(&root)).is_err());
        fs::write(root.join("Cargo.lock"), b"source").unwrap();
        write_new(&root.join("crates/catabolic-cli/src/new.rs"), b"new").unwrap();
        assert!(verify(&out, &hash, Some(&root)).is_err());
    }
    #[test]
    fn changed_binary_and_swapped_roles_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let out = tmp.path().join("out");
        let hash = fixture(&root, &out);
        fs::write(out.join("bin/catabolic"), b"changed").unwrap();
        assert!(verify(&out, &hash, None).is_err());
        fs::write(out.join("bin/catabolic"), b"companion").unwrap();
        fs::write(out.join("bin/catabolic-native"), b"legacy").unwrap();
        assert!(verify(&out, &hash, None).is_err());
    }
    #[test]
    fn missing_nonexecutable_and_unexpected_payloads_are_rejected() {
        for change in ["missing", "mode", "python", "empty-directory", "incomplete"] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("root");
            let out = tmp.path().join("out");
            let hash = fixture(&root, &out);
            match change {
                "missing" => fs::remove_file(out.join("bin/catabolic")).unwrap(),
                "mode" => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(
                            out.join("bin/catabolic"),
                            fs::Permissions::from_mode(0o644),
                        )
                        .unwrap();
                    }
                }
                "python" => write_new(&out.join("runtime.py"), b"unexpected").unwrap(),
                "empty-directory" => fs::create_dir(out.join("extra")).unwrap(),
                _ => write_new(&out.join("INCOMPLETE"), b"failed").unwrap(),
            }
            assert!(verify(&out, &hash, None).is_err(), "{change}");
        }
    }
    #[test]
    fn manifest_anchor_and_claim_changes_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let out = tmp.path().join("out");
        let hash = fixture(&root, &out);
        assert!(verify(&out, &"0".repeat(64), None).is_err());
        let mut m: Value = serde_json::from_slice(&fs::read(out.join(MANIFEST)).unwrap()).unwrap();
        m["python_free_runtime_accepted"] = json!(true);
        fs::write(out.join(MANIFEST), serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(verify(&out, &hash, None).is_err());
        let new_hash = identity(&out.join(MANIFEST)).unwrap().sha256;
        assert!(verify(&out, &new_hash, None).is_err());
    }
    #[test]
    fn no_overwrites_and_no_stale_success_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        reserve_output(&out).unwrap();
        write_new(&out.join(MANIFEST), b"old").unwrap();
        assert!(reserve_output(&out).is_err());
        assert_eq!(fs::read(out.join(MANIFEST)).unwrap(), b"old");
        let report = tmp.path().join("smoke.json");
        write_new(&report, b"old receipt").unwrap();
        assert!(smoke(&out, "ignored", &report).is_err());
        assert_eq!(fs::read(&report).unwrap(), b"old receipt");
        let failed = tmp.path().join("failed");
        assert!(prepare(&tmp.path().join("missing-source"), &failed).is_err());
        assert!(!failed.join(MANIFEST).exists());
        assert!(failed.join("INCOMPLETE").exists());
    }
    #[test]
    fn cargo_artifacts_require_exact_package_source_and_two_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        sources(tmp.path());
        write_new(
            &tmp.path().join("crates/catabolic-cli/src/legacy_main.rs"),
            b"legacy",
        )
        .unwrap();
        let message = |name: &str, src: &str, id: &str| {
            json!({"reason":"compiler-artifact","package_id":id,"target":{"name":name,"kind":["bin"],"src_path":tmp.path().join("crates/catabolic-cli/src").join(src)},"executable":"/fixture/bin"}).to_string()
        };
        let a = message("catabolic-native", "main.rs", "cli");
        let b = message("catabolic-legacy-native", "legacy_main.rs", "cli");
        artifact_paths(format!("{a}\n{b}\n").as_bytes(), "cli", tmp.path()).unwrap();
        assert!(artifact_paths(a.as_bytes(), "cli", tmp.path()).is_err());
        assert!(artifact_paths(format!("{a}\n{b}\n{a}").as_bytes(), "cli", tmp.path()).is_err());
        assert!(
            artifact_paths(
                format!(
                    "{a}\n{}",
                    message("catabolic-legacy-native", "main.rs", "cli")
                )
                .as_bytes(),
                "cli",
                tmp.path()
            )
            .is_err()
        );
        assert!(
            artifact_paths(
                format!(
                    "{a}\n{}",
                    message("catabolic-legacy-native", "legacy_main.rs", "other")
                )
                .as_bytes(),
                "cli",
                tmp.path()
            )
            .is_err()
        );
    }
    #[test]
    #[cfg(unix)]
    fn source_and_payload_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let out = tmp.path().join("out");
        let hash = fixture(&root, &out);
        symlink("catabolic", out.join("bin/alias")).unwrap();
        assert!(verify(&out, &hash, None).is_err());
        symlink("main.rs", root.join("crates/catabolic-cli/src/alias.rs")).unwrap();
        assert!(source_snapshot(&root).is_err());
    }
    #[test]
    fn process_output_and_time_are_bounded() {
        assert!(
            command_output(
                Command::new("/bin/sh").args(["-c", "printf too-much-output"]),
                5,
                2
            )
            .is_err()
        );
        assert!(command_output(Command::new("/bin/sh").args(["-c", "sleep 5"]), 0, 1024).is_err());
    }
}
