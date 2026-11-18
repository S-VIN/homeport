// The build script of every crate with a `migrations/` folder (its build.rs
// includes this file): embeds the folder's `*.sql` files in name order for
// `homeport_kernel::migrations!()`. A new file rebuilds the crate.
fn main() {
    let env = |name| std::env::var(name).expect("set by Cargo");
    let dir = std::path::Path::new(&env("CARGO_MANIFEST_DIR")).join("migrations");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("migrations/ folder")
        .map(|e| e.expect("migrations/ entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    let items: String = files
        .iter()
        .map(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).expect("UTF-8 file name");
            format!("({name:?}, include_str!({:?})),\n", p.display().to_string())
        })
        .collect();
    let out = std::path::Path::new(&env("OUT_DIR")).join("migrations.rs");
    std::fs::write(out, format!("&[\n{items}]")).expect("write migrations.rs");
}
