use std::{env, process::Command};
fn main() {
    for key in ["RUSTC", "NBREQ_R4_SOURCE"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let target = env::var("TARGET").expect("Cargo target");
    println!("cargo:rustc-env=R4_TARGET={target}");
    let rustc = Command::new(env::var_os("RUSTC").expect("compiler"))
        .arg("--version")
        .output()
        .expect("compiler version");
    assert!(rustc.status.success());
    println!(
        "cargo:rustc-env=R4_RUSTC={}",
        String::from_utf8(rustc.stdout)
            .expect("version UTF-8")
            .trim()
    );
    println!(
        "cargo:rustc-env=R4_SOURCE={}",
        env::var("NBREQ_R4_SOURCE").unwrap_or_else(|_| "unfrozen-development".to_owned())
    );
}
