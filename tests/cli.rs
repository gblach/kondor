use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

fn write(path: &Path, size: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![0; size]).unwrap();
}

/// A Cargo project with a Node project nested inside it.
fn make_projects(root: &Path) {
    write(&root.join("api/Cargo.toml"), 0);
    write(&root.join("api/target/debug/api"), 2048);
    write(&root.join("api/web/package.json"), 0);
    write(&root.join("api/web/node_modules/dep/index.js"), 1024);
}

fn kondor(root: &Path, args: &[&str]) -> Output {
    // stdin and stderr are not a terminal here, like in a script or a CI job.
    Command::new(env!("CARGO_BIN_EXE_kondor"))
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn dry_run_lists_projects_and_removes_nothing() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    make_projects(root);

    let output = kondor(root, &["--dry-run"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "Would clean ./api (Cargo) 2.0 KiB\n\
         Would clean ./api/web (Node) 1.0 KiB\n\
         Would free 3.0 KiB\n"
    );
    assert!(root.join("api/target").exists());
    assert!(root.join("api/web/node_modules").exists());
}

#[test]
fn yes_cleans_nested_projects() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    make_projects(root);

    let output = kondor(root, &["--yes"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "Cleaned ./api (Cargo) 2.0 KiB\n\
         Cleaned ./api/web (Node) 1.0 KiB\n\
         Freed 3.0 KiB\n"
    );
    assert!(!root.join("api/target").exists());
    assert!(!root.join("api/web/node_modules").exists());
    assert!(root.join("api/Cargo.toml").exists());
    assert!(root.join("api/web/package.json").exists());
}

#[test]
fn asking_needs_a_terminal() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    make_projects(root);

    let output = kondor(root, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(root.join("api/target").exists());
}

#[test]
fn reports_when_nothing_is_found() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("notes/todo.txt"), 10);

    let output = kondor(root, &["--yes"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "No projects with artifacts found.\n");
}

#[test]
fn unreadable_directory_argument_is_an_error() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    make_projects(root);
    write(&root.join("file"), 0);

    for bad in ["missing", "file"] {
        let output = kondor(root, &["--yes", "api", bad]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(stdout(&output), "");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with(&format!("Error: cannot scan {bad}: ")));
    }
    assert!(root.join("api/target").exists());
}

#[test]
fn failed_removal_sets_the_exit_code() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("old/go.mod"), 0);
    write(&root.join("old/vendor/modules.txt"), 0);
    // A read-only project directory keeps its `vendor` entry from being removed.
    fs::set_permissions(root.join("old"), fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(root.join("old/probe"), b"").is_ok() {
        // Running as root ignores permissions, so the failure cannot happen.
        return;
    }

    let output = kondor(root, &["--yes"]);
    // TempDir cannot delete the leftover `vendor` while its parent is read-only.
    fs::set_permissions(root.join("old"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "Failed ./old (Go)\nFreed 0 B\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to remove ./old/vendor"));
}

#[test]
fn force_removes_read_only_directories() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("old/go.mod"), 0);
    write(&root.join("old/vendor/mod/x"), 10);
    fs::set_permissions(
        root.join("old/vendor/mod"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();

    let output = kondor(root, &["--yes", "--force"]);
    assert!(output.status.success());
    assert!(!root.join("old/vendor").exists());
}

#[test]
fn kondor_file_excludes_are_kept() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("app/CMakeLists.txt"), 0);
    write(&root.join("app/build/handwritten.sh"), 10);
    write(&root.join("app/out/x"), 10);
    fs::write(root.join("app/.kondor"), "out\n!build\n").unwrap();

    let output = kondor(root, &["--yes"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "Cleaned ./app (C/C++, .kondor) 10 B\nFreed 10 B\n"
    );
    assert!(root.join("app/build/handwritten.sh").exists());
    assert!(!root.join("app/out").exists());
}

fn set_mtime(path: &Path, time: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn older_skips_recently_changed_projects() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("old/Cargo.toml"), 0);
    write(&root.join("old/target/x"), 10);
    write(&root.join("new/Cargo.toml"), 0);
    write(&root.join("new/target/x"), 10);
    let year_ago = SystemTime::now() - Duration::from_secs(400 * 86400);
    set_mtime(&root.join("old/Cargo.toml"), year_ago);
    set_mtime(&root.join("old/target/x"), year_ago);
    set_mtime(&root.join("new/Cargo.toml"), year_ago);

    let output = kondor(root, &["--yes", "--older", "1y"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "Cleaned ./old (Cargo) 10 B\nFreed 10 B\n");
    assert!(root.join("new/target/x").exists());
}

#[test]
fn exclude_skips_matching_directories() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    make_projects(root);
    write(&root.join("keep/Cargo.toml"), 0);
    write(&root.join("keep/target/x"), 10);
    write(&root.join("lib/Cargo.toml"), 0);
    write(&root.join("lib/target/x"), 10);
    write(&root.join("ruby/Gemfile"), 0);
    write(&root.join("ruby/vendor/bundle/x"), 10);

    // Several values after one option, like a pattern the shell has expanded.
    let output = kondor(root, &["-n", "--exclude", "api", "keep", "ruby", "--", "."]);
    assert_eq!(
        stdout(&output),
        "Would clean ./lib (Cargo) 10 B\nWould free 10 B\n"
    );

    let args = ["--yes", "-e", "keep", "api/w*", "-e", "**/vendor"];
    let output = kondor(root, &args);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "Cleaned ./api (Cargo) 2.0 KiB\n\
         Cleaned ./lib (Cargo) 10 B\n\
         Freed 2.0 KiB\n"
    );
    assert!(root.join("keep/target/x").exists());
    assert!(root.join("api/web/node_modules").exists());
    assert!(root.join("ruby/vendor/bundle/x").exists());

    let output = kondor(root, &["--dry-run", "--exclude", "*/target"]);
    assert_eq!(
        stdout(&output),
        "Would clean ./api/web (Node) 1.0 KiB\n\
         Would clean ./ruby (Ruby) 10 B\n\
         Would free 1.0 KiB\n"
    );
}
