use std::{env, fs, path::Path};

#[path = "src/schema_versions.rs"]
mod schema_versions;
use schema_versions::{FIRST_NATIVE_SCHEMA_VERSION, NATIVE_SCHEMA_VERSION, SCHEMA_VERSION};

fn main() {
    println!("cargo:rerun-if-changed=src/schema_versions.rs");
    let root = Path::new("resources/migrations");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut paths = fs::read_dir(root)
        .expect("migration directory")
        .map(|entry| entry.expect("migration entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths.len(),
        NATIVE_SCHEMA_VERSION as usize,
        "exactly migrations 001 through the latest native target are required"
    );
    let mut legacy = "pub static MIGRATIONS: &[Migration] = &[\n".to_string();
    let mut native = "pub static NATIVE_MIGRATIONS: &[Migration] = &[\n".to_string();
    for (index, path) in paths.iter().enumerate() {
        let filename = path
            .file_name()
            .unwrap()
            .to_str()
            .expect("UTF-8 migration name");
        let prefix = format!("{:03}_", index + 1);
        assert!(
            path.is_file() && filename.starts_with(&prefix) && filename.len() > 8,
            "migration versions must be unique and contiguous: expected {prefix}, found {filename}"
        );
        let version = index + 1;
        let absolute = fs::canonicalize(path).unwrap();
        let entry = format!(
            "Migration {{ version: {version}, filename: {filename:?}, sql: include_str!({:?}) }},\n",
            absolute.to_str().unwrap()
        );
        if version <= SCHEMA_VERSION as usize {
            legacy.push_str(&entry);
        }
        native.push_str(&entry);
        println!("cargo:rerun-if-changed={}", path.display());
    }
    legacy.push_str("];\n");
    native.push_str("];\n");
    let mut references = format!(
        "static NATIVE_SCHEMA_REFERENCES: [NativeSchemaReference; {}] = [\n",
        NATIVE_SCHEMA_VERSION - FIRST_NATIVE_SCHEMA_VERSION + 1
    );
    // Every admitted native version has its own immutable historical signature.
    // Adding a migration without its versioned signature must fail the build.
    for version in FIRST_NATIVE_SCHEMA_VERSION..=NATIVE_SCHEMA_VERSION {
        let path = Path::new("resources").join(format!("native-schema-{version}.json"));
        let absolute = fs::canonicalize(&path).expect("native schema signature resource");
        references.push_str(&format!(
            "NativeSchemaReference {{ version: {version}, json: include_str!({:?}), parsed: OnceLock::new() }},\n",
            absolute.to_str().expect("UTF-8 native signature path")
        ));
        println!("cargo:rerun-if-changed={}", path.display());
    }
    references.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("migrations.rs"),
        legacy + &native + &references,
    )
    .unwrap();
}
