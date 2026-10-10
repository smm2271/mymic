use std::process::Command;
fn git(args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .output()
        .expect("git required for build provenance");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!(
        "cargo:rustc-env=MIC_BUILD_COMMIT={}",
        git(&["rev-parse", "HEAD"])
    );
    println!(
        "cargo:rustc-env=MIC_BUILD_DIRTY={}",
        !git(&["status", "--porcelain", "--untracked-files=normal"]).is_empty()
    );
}
