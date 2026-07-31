// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
}

fn git_stdout(args: &[&str]) -> Option<String> {
    git(args).and_then(|output| {
        String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn git_dir() -> Option<PathBuf> {
    git_stdout(&["rev-parse", "--git-dir"]).map(PathBuf::from)
}

fn git_common_dir() -> Option<PathBuf> {
    git_stdout(&["rev-parse", "--git-common-dir"]).map(PathBuf::from)
}

fn git_head_path() -> Option<PathBuf> {
    git_stdout(&["rev-parse", "--git-path", "HEAD"]).map(PathBuf::from)
}

fn git_dirty() -> bool {
    git(&["status", "--porcelain", "--untracked-files=normal"])
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(true)
}

fn register_rerun_paths(git_dir: &Path, common_dir: &Path, head_path: &Path) {
    println!("cargo:rerun-if-changed={}", head_path.display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    println!(
        "cargo:rerun-if-changed={}",
        common_dir.join("packed-refs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        common_dir.join("refs").display()
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
}

fn main() {
    let commit = git_stdout(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git_dirty();

    println!("cargo:rustc-env=THERMALWRITER_GIT_COMMIT={commit}");
    println!(
        "cargo:rustc-env=THERMALWRITER_GIT_DIRTY={}",
        u8::from(dirty)
    );

    if let (Some(git_dir), Some(common_dir), Some(head_path)) =
        (git_dir(), git_common_dir(), git_head_path())
    {
        register_rerun_paths(&git_dir, &common_dir, &head_path);
    } else {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=src");
    }
}
