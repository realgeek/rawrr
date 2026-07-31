use std::process::Command;

fn main() {
    // Tell Cargo to rerun this script when the hash source changes
    println!("cargo:rerun-if-env-changed=GIT_COMMIT_HASH");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");

    // Prefer a value injected from outside (e.g. Docker build arg) so that
    // builds without a .git directory (CI, Docker) still get a meaningful hash.
    let hash = std::env::var("GIT_COMMIT_HASH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });

    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", hash);
}
