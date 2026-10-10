use std::{env, fs, path::Path};
fn main() {
    let root = Path::new("resources/migrations");
    let mut paths = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut code = "pub static MIGRATIONS: &[Migration] = &[\n".to_string();
    for path in paths {
        let filename = path.file_name().unwrap().to_str().unwrap();
        if !filename.ends_with(".sql") {
            continue;
        }
        let version: u32 = filename[..3].parse().unwrap();
        let absolute = fs::canonicalize(&path).unwrap();
        code.push_str(&format!("Migration {{ version: {version}, filename: {filename:?}, sql: include_str!({:?}) }},\n", absolute.to_str().unwrap()));
        println!("cargo:rerun-if-changed={}", path.display());
    }
    code.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("migrations.rs"),
        code,
    )
    .unwrap();
}
