#![cfg(feature = "native")]

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

#[test]
fn review_p2_package_contains_compile_time_fuzz_seed_dependencies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("crate root must exist");
    // Inspect Cargo's actual archive inventory without publishing, modifying the manifest, or
    // depending on a registry connection. A repository checkout can otherwise hide missing seeds.
    let inventory = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["package", "--list", "--allow-dirty", "--offline"])
        .output()
        .expect("Cargo package inventory must run");
    assert!(
        inventory.status.success(),
        "Cargo package inventory failed: {}",
        String::from_utf8_lossy(&inventory.stderr)
    );
    let packaged: HashSet<String> = String::from_utf8(inventory.stdout)
        .expect("Cargo package paths must be UTF-8")
        .lines()
        .map(|path| path.replace('\\', "/"))
        .collect();

    let mut referenced = Vec::new();
    for source in [
        "src/backend/native_dns/tests.rs",
        "src/backend/native_http/tests.rs",
    ] {
        assert!(
            packaged.contains(source),
            "fixture consumer must be packaged"
        );
        let source_path = root.join(source);
        let text = std::fs::read_to_string(&source_path).expect("fixture consumer must read");
        for invocation in text.split("include_bytes!(").skip(1) {
            let literal = invocation.trim_start();
            let quoted = literal
                .strip_prefix('"')
                .expect("fixture uses a literal path");
            let (relative, _) = quoted
                .split_once('"')
                .expect("fixture path closes its quote");
            let dependency = source_path
                .parent()
                .expect("source directory")
                .join(relative)
                .canonicalize()
                .expect("referenced seed exists in this checkout");
            let relative = dependency
                .strip_prefix(&root)
                .expect("fixture dependency remains within the crate")
                .to_str()
                .expect("fixture path must be UTF-8")
                .replace('\\', "/");
            referenced.push(relative);
        }
    }
    assert!(
        !referenced.is_empty(),
        "must exercise real compile-time seed dependencies"
    );
    let missing: Vec<_> = referenced
        .iter()
        .filter(|path| !packaged.contains(*path))
        .collect();
    assert!(
        missing.is_empty(),
        "packaged unit tests reference compile-time fixtures omitted from the crate archive: {missing:#?}"
    );
}
