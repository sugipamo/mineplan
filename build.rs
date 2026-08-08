use std::env;
use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=MINEPLAN_BUILD_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{}", reference.trim());
        }
    }

    let version = env::var("MINEPLAN_BUILD_VERSION")
        .ok()
        .filter(|version| !version.trim().is_empty())
        .or_else(git_version)
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
    println!("cargo:rustc-env=MINEPLAN_BUILD_VERSION={version}");
}

fn git_version() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
