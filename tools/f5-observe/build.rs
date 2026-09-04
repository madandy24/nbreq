use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_owned());
    println!("cargo:rustc-env=NBREQ_F5_TARGET={target}");

    let rustc = env::var_os("RUSTC")
        .and_then(|rustc| Command::new(rustc).arg("--version").output().ok())
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().to_owned())
        .unwrap_or_else(|| "unknown-rustc".to_owned());
    println!("cargo:rustc-env=NBREQ_F5_RUSTC={rustc}");
}
