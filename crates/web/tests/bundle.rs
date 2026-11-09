use std::path::{Path, PathBuf};

const SOURCE_FILES: [&str; 6] = [
    "index.html",
    "package.json",
    "bun.lock",
    "tsconfig.json",
    "vite.config.ts",
    "source-hash.ts",
];
const SOURCE_DIRS: [&str; 1] = ["src"];

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let relative = path.strip_prefix(root).unwrap();
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
}

fn source_hash(root: &Path) -> String {
    let mut files: Vec<String> = SOURCE_FILES
        .iter()
        .filter(|f| root.join(f).is_file())
        .map(|f| f.to_string())
        .collect();
    for dir in SOURCE_DIRS {
        walk(root, &root.join(dir), &mut files);
    }
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ *byte as u64).wrapping_mul(0x100000001b3);
        }
    };
    for file in files {
        feed(file.as_bytes());
        feed(&[0]);
        feed(&std::fs::read(root.join(&file)).unwrap());
        feed(&[0]);
    }
    format!("{hash:016x}")
}

fn ui() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

#[test]
fn the_committed_dashboard_build_matches_its_sources() {
    let built = std::fs::read_to_string(ui().join("dist/index.html")).unwrap();
    let stamp = built
        .split("<meta name=\"haze-source\" content=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("dist/index.html has no haze-source stamp; rebuild it with `bun run build`");
    assert_eq!(
        stamp,
        source_hash(&ui()),
        "crates/web/ui/dist/index.html is older than crates/web/ui; run `cd crates/web/ui && bun install && bun run build` and commit dist"
    );
}

#[test]
fn the_dashboard_build_is_one_self_contained_file() {
    let built = std::fs::read_to_string(ui().join("dist/index.html")).unwrap();
    assert!(
        !built.contains("<script type=\"module\" src="),
        "scripts are inlined"
    );
    assert!(
        !built.contains("<link rel=\"stylesheet\""),
        "styles are inlined"
    );
    for line in built.lines() {
        for url in ["src=\"http", "href=\"http", "url(http", "import(\"http"] {
            assert!(!line.contains(url), "external asset in the bundle: {url}");
        }
    }
}
