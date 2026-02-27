use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/renderer/shader.metal");

    // Extract rustc commit hash for version output
    let output = Command::new("rustc")
        .args(["-vV"])
        .output()
        .expect("failed to run rustc -vV");
    let verbose = String::from_utf8(output.stdout).unwrap();
    let commit = verbose
        .lines()
        .find_map(|line| line.strip_prefix("commit-hash: "))
        .unwrap_or("unknown");
    println!("cargo:rustc-env=TTY_RUSTC_COMMIT={commit}");
}
