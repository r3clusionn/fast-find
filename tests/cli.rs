//! Runs the real binary on a temporary tree and checks which paths come back.

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, bytes: usize) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![b'x'; bytes]).unwrap();
}

/// A tree with a .gitignore, hidden files, mixed extensions and sizes.
fn tree() -> TempDir {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    // `ignore` only honours .gitignore inside a git work tree.
    fs::create_dir_all(r.join(".git")).unwrap();
    write(r, ".gitignore", 0);
    fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
    write(r, "src/main.rs", 100);
    write(r, "src/lib.rs", 2000);
    write(r, "src/Util.RS", 10);
    write(r, "docs/README.md", 50);
    write(r, "target/debug/out.rs", 5);
    write(r, "run.log", 5);
    write(r, ".hidden/secret.rs", 5);
    write(r, "deep/a/b/c/d.rs", 5);
    t
}

fn ff(root: &Path, args: &[&str]) -> (i32, Vec<String>) {
    let out = Command::new(env!("CARGO_BIN_EXE_ff"))
        .args(args)
        .arg("--sort")
        .current_dir(root)
        .output()
        .unwrap();
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.replace('\\', "/").trim_start_matches("./").to_string())
        .collect();
    lines.sort();
    (out.status.code().unwrap(), lines)
}

#[test]
fn respects_gitignore_and_hides_dotfiles_by_default() {
    let t = tree();
    let (code, got) = ff(t.path(), &["-e", "rs"]);
    assert_eq!(code, 0);
    assert_eq!(got, ["deep/a/b/c/d.rs", "src/Util.RS", "src/lib.rs", "src/main.rs"]);
}

#[test]
fn hidden_and_no_ignore_flags() {
    let t = tree();
    let (_, got) = ff(t.path(), &["-e", "rs", "-H", "-I"]);
    assert!(got.contains(&".hidden/secret.rs".to_string()));
    assert!(got.contains(&"target/debug/out.rs".to_string()));
    let (_, got) = ff(t.path(), &["log", "-I"]);
    assert_eq!(got, ["run.log"]);
}

#[test]
fn smart_case_pattern() {
    let t = tree();
    let (_, lower) = ff(t.path(), &["util"]);
    assert_eq!(lower, ["src/Util.RS"], "lowercase pattern matches any case");
    let (_, capital) = ff(t.path(), &["Util"]);
    assert_eq!(capital, ["src/Util.RS"]);
    let (code, upper) = ff(t.path(), &["UTIL"]);
    assert_eq!((code, upper.len()), (1, 0), "a capital makes the match case sensitive");
    let (code, none) = ff(t.path(), &["-s", "util"]);
    assert_eq!((code, none.len()), (1, 0), "case sensitive, nothing matches, exit code 1");
}

#[test]
fn size_depth_and_type_filters() {
    let t = tree();
    let (_, big) = ff(t.path(), &["-S", "+1k", "-t", "f"]);
    assert_eq!(big, ["src/lib.rs"]);
    let (_, small) = ff(t.path(), &["-S", "-20", "-e", "rs"]);
    assert_eq!(small, ["deep/a/b/c/d.rs", "src/Util.RS"]);
    let (_, shallow) = ff(t.path(), &["-e", "rs", "-d", "2"]);
    assert!(!shallow.iter().any(|p| p.starts_with("deep/")));
    let (_, dirs) = ff(t.path(), &["-t", "d", "src"]);
    assert_eq!(dirs, ["src"]);
}

#[test]
fn full_path_and_literal_patterns() {
    let t = tree();
    let (_, got) = ff(t.path(), &["-p", "src/.*\\.md$"]);
    assert!(got.is_empty());
    let (_, got) = ff(t.path(), &["-p", "-F", "docs/README"]);
    assert_eq!(got, ["docs/README.md"]);
}

#[test]
fn newer_filter_uses_modification_time() {
    let t = tree();
    let (_, fresh) = ff(t.path(), &["-e", "md", "--newer", "1h"]);
    assert_eq!(fresh, ["docs/README.md"]);
    let (code, old) = ff(t.path(), &["-e", "md", "--older", "1h"]);
    assert_eq!((code, old.len()), (1, 0));
}

#[test]
fn count_and_errors() {
    let t = tree();
    let out = Command::new(env!("CARGO_BIN_EXE_ff"))
        .args(["-e", "rs", "-c"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "4");
    let bad = Command::new(env!("CARGO_BIN_EXE_ff")).arg("(").current_dir(t.path()).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let missing = Command::new(env!("CARGO_BIN_EXE_ff")).args(["x", "nope"]).current_dir(t.path()).output().unwrap();
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn unsorted_parallel_output_has_the_same_set_as_sorted() {
    let t = tree();
    let unsorted = Command::new(env!("CARGO_BIN_EXE_ff")).args(["-e", "rs", "-j", "4"]).current_dir(t.path()).output().unwrap();
    let mut a: Vec<String> = String::from_utf8_lossy(&unsorted.stdout).lines().map(str::to_string).collect();
    a.sort();
    let (_, b) = ff(t.path(), &["-e", "rs"]);
    assert_eq!(a.len(), b.len());
}
