use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::json;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_fastjj");

struct Fixture {
    root: TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let config = root.path().join("config.toml");
        fs::write(&config, "[user]\nname = 'Privacy Test'\nemail = 'test@example.invalid'\n[ui]\ncolor = 'never'\npaginate = 'never'\n").unwrap();
        Self { root, config }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(BIN);
        command
            .current_dir(cwd)
            .env("JJ_CONFIG", &self.config)
            .env("RAYON_NUM_THREADS", "2")
            .env_remove("JJ_CONFIG_TOML")
            .env_remove("COMPLETE")
            .stdin(Stdio::null());
        command
    }

    fn output(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd).args(args).output().unwrap()
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> String {
        success(self.output(cwd, args))
    }

    fn init(&self, name: &str) -> PathBuf {
        self.run(self.root.path(), &["git", "init", name]);
        self.path(name)
    }

    fn private(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut child = self
            .command(self.root.path())
            .arg("--request-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = serde_json::to_vec(&json!({"cwd":cwd,"args":args})).unwrap();
        child.stdin.take().unwrap().write_all(&request).unwrap();
        child.wait_with_output().unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> String {
    assert!(output.status.success(), "{}", text(&output));
    text(&output)
}

#[test]
fn local_history_diff_bookmarks_rebase_restore_and_undo() {
    let f = Fixture::new();
    let repo = f.init("private-project");
    fs::write(repo.join("hello.txt"), "first\n").unwrap();
    assert!(f.run(&repo, &["status"]).contains("hello.txt"));
    assert!(f.run(&repo, &["diff", "--git"]).contains("+first"));
    f.run(&repo, &["describe", "-m", "first change"]);
    f.run(&repo, &["bookmark", "create", "main"]);
    f.run(&repo, &["new", "-m", "second change"]);
    fs::write(repo.join("hello.txt"), "second\n").unwrap();
    assert!(
        f.run(&repo, &["file", "show", "hello.txt"])
            .contains("second")
    );
    assert!(
        f.run(
            &repo,
            &["log", "--no-graph", "-r", "all()", "-T", "description"]
        )
        .contains("first change")
    );
    f.run(&repo, &["new", "root()", "-m", "parallel change"]);
    fs::write(repo.join("parallel.txt"), "parallel\n").unwrap();
    f.run(&repo, &["status"]);
    f.run(&repo, &["rebase", "-s", "@", "-d", "main"]);
    assert!(repo.join("hello.txt").exists());
    assert!(repo.join("parallel.txt").exists());
    f.run(&repo, &["restore", "--from", "main", "parallel.txt"]);
    assert!(!repo.join("parallel.txt").exists());
    f.run(&repo, &["undo"]);
    assert!(repo.join("parallel.txt").exists());
    assert!(
        f.run(&repo, &["op", "log", "--limit", "5"])
            .contains("restore")
    );
    assert!(f.run(&repo, &["bookmark", "list"]).contains("main"));
}

#[test]
fn private_stdin_init_and_status_keep_argv_clean() {
    let f = Fixture::new();
    let repo = f.path("secret-customer-repository");
    success(f.private(f.root.path(), &["git", "init", repo.to_str().unwrap()]));
    fs::write(repo.join("secret-path.txt"), "private\n").unwrap();

    let mut child = f
        .command(f.root.path())
        .arg("--request-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Observe the running process from the test harness (not from fastjj).
    // Wide ps output is available on both macOS and Linux and never shows env.
    let observed = Command::new("ps")
        .args(["-ww", "-p", &child.id().to_string(), "-o", "args="])
        .output()
        .unwrap();
    assert!(observed.status.success());
    let argv = String::from_utf8_lossy(&observed.stdout);
    assert!(argv.contains("--request-stdin"));
    assert!(!argv.contains("secret-customer"));
    assert!(!argv.contains(repo.to_str().unwrap()));
    let request = serde_json::to_vec(&json!({"cwd":repo,"args":["status"]})).unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    assert!(success(child.wait_with_output().unwrap()).contains("secret-path.txt"));
}

#[test]
fn git_clone_fetch_push_are_in_process() {
    let f = Fixture::new();
    let origin = f.init("secret-origin");
    fs::write(origin.join("hello.txt"), "initial\n").unwrap();
    f.run(&origin, &["describe", "-m", "initial"]);
    f.run(&origin, &["bookmark", "create", "main"]);
    f.run(&origin, &["new"]);
    f.run(&origin, &["git", "export"]);
    let git_dir = origin.join(".jj/repo/store/git");
    // jj's private bare store has an unborn default HEAD until it is given a
    // default branch. Model a remote advertising main, not an empty remote.
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let clone = f.path("secret-clone");
    success(f.private(
        f.root.path(),
        &[
            "git",
            "clone",
            git_dir.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        fs::read_to_string(clone.join("hello.txt")).unwrap(),
        "initial\n"
    );
    fs::write(clone.join("hello.txt"), "updated\n").unwrap();
    f.run(&clone, &["describe", "-m", "updated"]);
    f.run(&clone, &["bookmark", "set", "main"]);
    f.run(&clone, &["git", "push", "--bookmark", "main"]);
    f.run(&origin, &["git", "import"]);
    assert!(
        f.run(&origin, &["log", "-r", "main", "-T", "description"])
            .contains("updated")
    );
    f.run(&origin, &["rebase", "-r", "@", "-d", "main"]);
    fs::write(origin.join("new.txt"), "from origin\n").unwrap();
    f.run(&origin, &["describe", "-m", "fetch me"]);
    f.run(&origin, &["bookmark", "set", "main", "--allow-backwards"]);
    f.run(&origin, &["git", "export"]);
    f.run(&clone, &["git", "fetch"]);
    assert!(
        f.run(&clone, &["log", "-r", "main@origin", "-T", "description"])
            .contains("fetch me")
    );
}

#[test]
fn config_cannot_enable_subprocess_transport_or_pagers() {
    let f = Fixture::new();
    let repo = f.init("config-test");
    for (key, attempt, expected) in [
        ("git.subprocess", "true", "false"),
        ("ui.pager", "malicious-pager", ":builtin"),
        ("ui.editor", "external-editor", ":builtin"),
        ("core.fsmonitor", "watchman", "none"),
        ("ui.diff-editor", "external-diff", ":builtin"),
        ("ui.merge-editor", "external-merge", ":builtin"),
    ] {
        f.run(&repo, &["config", "set", "--repo", key, attempt]);
        let config = format!("{key}={attempt}");
        let output = f.output(&repo, &["--config", &config, "config", "get", key]);
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            expected,
            "key={key}"
        );
    }
}

#[test]
fn user_config_is_preserved_before_and_after_subcommands() {
    let f = Fixture::new();
    let repo = f.init("user-options");
    for args in [
        vec![
            "--config",
            "user.name='Override'",
            "config",
            "get",
            "user.name",
        ],
        vec![
            "config",
            "get",
            "user.name",
            "--config",
            "user.name='Override'",
        ],
    ] {
        let output = f.output(&repo, &args);
        assert!(output.status.success(), "{}", text(&output));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Override");
    }
    // The same preservation applies to in-memory private requests.
    let output = f.private(
        &repo,
        &[
            "--config",
            "user.name='Private Override'",
            "config",
            "get",
            "user.name",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Private Override"
    );
}

#[test]
fn external_commands_and_editors_fail_without_side_effects() {
    let f = Fixture::new();
    let repo = f.init("external-test");
    let marker = f.path("must-not-exist");
    let script = format!("touch {}", marker.display());
    let output = f.output(&repo, &["util", "exec", "--", "sh", "-c", &script]);
    assert!(!output.status.success());
    assert!(text(&output).contains("fastjj denies"));
    assert!(!marker.exists());
    fs::write(repo.join("file.txt"), "change\n").unwrap();
    // Deserialize one complete tool table. Separate leaf config layers can
    // shadow fields during typed table lookup. Use an absolute, existing shell
    // so the failure proves sandbox enforcement rather than PATH lookup.
    let editor = format!("merge-tools.escape={{program='/bin/sh',diff-args=['-c','{script}']}}");
    let configured = f.output(
        &repo,
        &[
            "--config",
            &editor,
            "config",
            "get",
            "merge-tools.escape.program",
        ],
    );
    assert!(configured.status.success(), "{}", text(&configured));
    assert_eq!(
        String::from_utf8_lossy(&configured.stdout).trim(),
        "/bin/sh"
    );
    let output = f.output(&repo, &["--config", &editor, "diff", "--tool", "escape"]);
    assert!(!output.status.success());
    let error = text(&output);
    assert!(
        error.contains("Operation not permitted") || error.contains("Permission denied"),
        "{error}"
    );
    assert!(!marker.exists());
}

#[test]
fn malformed_requests_fail_without_echoing_private_input() {
    let f = Fixture::new();
    let mut child = f
        .command(f.root.path())
        .arg("--request-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret-invalid-request")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!text(&output).contains("secret-invalid-request"));
    assert!(text(&output).contains("expected JSON"));
    let output = f.output(f.root.path(), &["--request-stdin", "status"]);
    assert!(!output.status.success());
    assert!(text(&output).contains("must be used alone"));
}

#[test]
fn help_version_and_privacy_status() {
    let f = Fixture::new();
    assert!(f.run(f.root.path(), &["--help"]).contains("fastjj"));
    assert!(f.run(f.root.path(), &["--version"]).contains("0.1.0"));
    assert!(
        f.run(f.root.path(), &["privacy"])
            .contains("Child processes and exec: denied")
    );
}
