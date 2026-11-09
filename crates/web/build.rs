use std::path::PathBuf;

fn main() {
    let bundle = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/dist/index.html");
    println!("cargo:rerun-if-changed={}", bundle.display());
    println!("cargo:rerun-if-changed=build.rs");
    if !bundle.is_file() {
        panic!(
            "the dashboard has not been built: {} is missing.\nRun `cd crates/web/ui && bun install && bun run build` (needs bun, https://bun.sh)",
            bundle.display()
        );
    }
}
