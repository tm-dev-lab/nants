//! `scripts/package.sh` (spec 110, T01): runs the real script in a temporary Git repository, with
//! fake `cargo`, `npm`, `zig`, `cargo-zigbuild`, `rustup`, `az` and a logging `git` wrapper on the
//! PATH, and checks the packages, the SHA-256, the `latest.json`, the release notes, the refusals
//! and that nothing uploads or tags. Nothing is really built and nothing leaves the machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Temporary folder of the test, removed at the end even on failure (`Drop` guard, CLAUDE.md).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("nants-package-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temporary folder");
    TempDir(dir)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("folder");
    }
    std::fs::write(path, body).expect("file");
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, body);
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

fn script(dir: &Path, name: &str, body: &str) {
    write_exec(&dir.join(name), &format!("#!/bin/sh\n{body}\n"));
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git")
}

fn real_git() -> PathBuf {
    ["/usr/bin/git", "/bin/git", "/usr/local/bin/git"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .expect("git on the system")
}

/// Fake `cargo zigbuild`: creates `target/<triple without glibc>/release/nants`; `cargo test`
/// follows `FAKE_TEST_FAIL`. Every call goes to `calls.log`; the tests also log `TMPDIR`, to prove
/// they ran through `scripts/test.sh`.
const FAKE_CARGO: &str = r#"echo "cargo $*" >> "$FAKE_LOG"
case "$1" in
  test)
    echo "tmpdir cargo $TMPDIR" >> "$FAKE_LOG"
    [ -z "${FAKE_TEST_FAIL:-}" ] || exit 101 ;;
  zigbuild)
    for a in "$@"; do case "$prev" in --target) t="$a" ;; esac; prev="$a"; done
    d="target/$(printf '%s' "$t" | sed 's/\.[0-9][0-9.]*$//')/release"
    mkdir -p "$d"
    printf 'binary %s\n' "$t" > "$d/nants"
    chmod +x "$d/nants" ;;
esac
exit 0"#;

/// Fake `npm`: logs the call and, for `npm test`, the `TMPDIR`.
const FAKE_NPM: &str = r#"echo "npm $*" >> "$FAKE_LOG"
[ "$1" = "test" ] && echo "tmpdir npm $TMPDIR" >> "$FAKE_LOG"
exit 0"#;

struct Fixture {
    base: TempDir,
    runs: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
    log: PathBuf,
}

/// Repository with the script, `Cargo.toml` (0.4.2), `CHANGELOG.md` and one commit. No `site/`:
/// the public repository does not have it.
fn fixture(tag: &str, changelog: &str) -> Fixture {
    let base = temp_dir(tag);
    let repo = base.0.join("repo");
    let bin = base.0.join("bin");
    let log = base.0.join("calls.log");
    let runs = base.0.join("test-runs");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let body = std::fs::read_to_string(root.join("scripts/package.sh")).expect("package.sh");
    write(&repo.join("scripts/package.sh"), &body);
    let test_sh = std::fs::read_to_string(root.join("scripts/test.sh")).expect("test.sh");
    write_exec(&repo.join("scripts/test.sh"), &test_sh);
    write(
        &repo.join("Cargo.toml"),
        "[package]\nname = \"nants\"\nversion = \"0.4.2\"\n\n[dependencies]\nserde = { version = \"1\" }\n",
    );
    write(&repo.join("CHANGELOG.md"), changelog);
    write(&repo.join("web/package.json"), "{}\n");
    write(&repo.join(".gitignore"), "target/\ndist-release/\n");
    assert!(git(&repo, &["init", "-q"]).status.success());
    assert!(git(&repo, &["add", "-A"]).status.success());
    assert!(git(&repo, &["commit", "-q", "-m", "initial"]).status.success());
    script(&bin, "cargo", FAKE_CARGO);
    script(&bin, "npm", FAKE_NPM);
    script(&bin, "cargo-zigbuild", "exit 0");
    script(&bin, "zig", "exit 0");
    script(&bin, "rustup", "echo \"rustup $*\" >> \"$FAKE_LOG\"\nexit 0");
    script(&bin, "az", "echo \"az $*\" >> \"$FAKE_LOG\"\nexit 0");
    script(&bin, "gh", "echo \"gh $*\" >> \"$FAKE_LOG\"\nexit 0");
    script(&bin, "git", &format!("echo \"git $*\" >> \"$FAKE_LOG\"\nexec {} \"$@\"", real_git().display()));
    Fixture { base, runs, repo, bin, log }
}

const CHANGELOG_OK: &str = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n- Later.\n\n## [0.4.2] - 2026-10-21\n\nFirst line.\n\n### Added\n\n- Something.\n- Another `thing`.\n\n## [0.4.1] - 2026-01-01\n\n### Added\n- Before.\n\n[0.4.2]: https://example.com\n";

fn run(f: &Fixture, args: &[&str], extra: &[(&str, &str)]) -> Output {
    run_with("sh", f, args, extra)
}

fn run_with(shell: &str, f: &Fixture, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(shell);
    cmd.arg(f.repo.join("scripts/package.sh"))
        .args(args)
        .current_dir(&f.repo)
        .env("PATH", format!("{}:/usr/bin:/bin", f.bin.display()))
        .env("HOME", &f.base.0)
        .env("FAKE_LOG", &f.log)
        .env("NANTS_TEST_TMP_BASE", &f.runs)
        .env("TMPDIR", &f.base.0)
        .env_remove("CARGO_TARGET_DIR");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("package.sh")
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn calls(f: &Fixture) -> String {
    std::fs::read_to_string(&f.log).unwrap_or_default()
}

fn manifest(f: &Fixture) -> serde_json::Value {
    let body = std::fs::read_to_string(f.repo.join("dist-release/v0.4.2/latest.json")).expect("latest.json");
    serde_json::from_str(&body).expect("json")
}

/// Nothing uploads, tags or pushes.
fn assert_no_publishing(log: &str) {
    for line in log.lines() {
        assert!(!line.starts_with("az "), "az called: {log}");
        assert!(!line.starts_with("gh "), "gh called: {log}");
        assert!(!line.starts_with("git tag") && !line.contains(" tag "), "git tag called: {log}");
        assert!(!line.starts_with("git push") && !line.contains(" push"), "git push called: {log}");
    }
}

#[test]
fn packages_both_targets_with_sha_and_the_default_github_manifest() {
    let f = fixture("ok", CHANGELOG_OK);
    let out = run(&f, &[], &[]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");

    let dir = f.repo.join("dist-release/v0.4.2");
    let m = manifest(&f);
    assert_eq!(m["version"], "0.4.2");
    assert!(m["date"].as_str().is_some_and(|d| d.ends_with('Z')), "{m}");
    assert_eq!(m["assets"].as_object().map(|a| a.len()), Some(2), "{m}");
    for name in ["x86_64-linux", "aarch64-linux"] {
        let file = format!("nants-0.4.2-{name}.tar.gz");
        let asset = &m["assets"][name];
        assert_eq!(
            asset["url"],
            format!("https://github.com/tm-dev-lab/nants/releases/download/v0.4.2/{file}"),
            "{m}"
        );
        let sum_line = std::fs::read_to_string(dir.join(format!("{file}.sha256"))).expect("sha256");
        let sum = sum_line.split_whitespace().next().unwrap_or("");
        assert_eq!(sum.len(), 64);
        assert_eq!(asset["sha256"], sum);
        assert_eq!(sum_line, format!("{sum}  {file}\n"));
        let check = Command::new("sha256sum")
            .arg("-c")
            .arg(format!("{file}.sha256"))
            .current_dir(&dir)
            .output()
            .expect("sha256sum");
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stdout));
        let list = Command::new("tar").arg("-tzvf").arg(dir.join(&file)).output().expect("tar");
        let listing = String::from_utf8_lossy(&list.stdout).to_string();
        assert!(listing.trim().ends_with(" nants"), "{listing}");
        assert!(listing.starts_with("-rwxr-xr-x 0/0"), "owner 0 and mode 755: {listing}");
    }
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("dist")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "latest.json",
            "nants-0.4.2-aarch64-linux.tar.gz",
            "nants-0.4.2-aarch64-linux.tar.gz.sha256",
            "nants-0.4.2-x86_64-linux.tar.gz",
            "nants-0.4.2-x86_64-linux.tar.gz.sha256",
        ]
    );

    let log = calls(&f);
    assert!(log.contains("cargo test"), "{log}");
    assert!(log.contains("npm test") && log.contains("npm run build"), "{log}");
    // Tests only through scripts/test.sh: their own temporary folder, removed at the end. No site.
    let runs = f.runs.display().to_string();
    let tmpdirs: Vec<&str> = log.lines().filter(|l| l.starts_with("tmpdir ")).collect();
    assert_eq!(tmpdirs.len(), 2, "cargo test and the web npm test: {log}");
    assert!(tmpdirs.iter().all(|l| l.contains(&format!("{runs}/run-"))), "{log}");
    let left = std::fs::read_dir(&f.runs).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "scripts/test.sh removes the folder of each run");
    // The tests run before the build.
    let first_build = log.find("cargo zigbuild").expect("zigbuild");
    assert!(log.find("cargo test").is_some_and(|i| i < first_build), "{log}");
    assert!(log.contains("--target x86_64-unknown-linux-gnu.2.31"), "{log}");
    assert!(log.contains("--target aarch64-unknown-linux-gnu.2.31"), "{log}");
    let rustup: Vec<&str> = log.lines().filter(|l| l.starts_with("rustup ")).collect();
    assert_eq!(
        rustup,
        ["rustup target add x86_64-unknown-linux-gnu", "rustup target add aarch64-unknown-linux-gnu"],
        "{log}"
    );
    assert_no_publishing(&log);
    let tags = git(&f.repo, &["tag", "-l"]);
    assert!(String::from_utf8_lossy(&tags.stdout).trim().is_empty(), "no tag is created");
}

#[test]
fn archives_are_reproducible() {
    let f = fixture("repro", CHANGELOG_OK);
    let file = f.repo.join("dist-release/v0.4.2/nants-0.4.2-x86_64-linux.tar.gz");
    assert!(run(&f, &["--skip-tests"], &[]).status.success());
    let first = std::fs::read(&file).expect("package");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(run(&f, &["--skip-tests"], &[]).status.success());
    assert_eq!(first, std::fs::read(&file).expect("package"), "same commit, same bytes");
}

#[test]
fn base_url_option_changes_the_manifest_urls() {
    let f = fixture("base", CHANGELOG_OK);
    let out = run(&f, &["--skip-tests", "--base-url", "https://mirror.example.com/nants/v0.4.2/"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let m = manifest(&f);
    assert_eq!(
        m["assets"]["aarch64-linux"]["url"],
        "https://mirror.example.com/nants/v0.4.2/nants-0.4.2-aarch64-linux.tar.gz",
        "{m}"
    );
    assert_eq!(
        m["assets"]["x86_64-linux"]["url"],
        "https://mirror.example.com/nants/v0.4.2/nants-0.4.2-x86_64-linux.tar.gz",
        "{m}"
    );
}

#[test]
fn notes_are_exactly_the_version_section() {
    let f = fixture("notes", CHANGELOG_OK);
    let notes = f.base.0.join("notes.md");
    let out = run(&f, &["--skip-tests", "--notes", notes.to_str().expect("utf8")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let body = std::fs::read_to_string(&notes).expect("notes");
    assert_eq!(body, "First line.\n\n### Added\n\n- Something.\n- Another `thing`.\n");

    // The last section goes to the end of the file, without the link definitions; a link that
    // only looks like one inside the text stays.
    let f = fixture(
        "notes-last",
        "# Changelog\n\n## [0.4.2] - 2026-10-21\n\n- Only, see [docs](https://x).\n\n[0.4.2]: https://example.com\n[Unreleased]: https://example.com/u\n",
    );
    let notes = f.base.0.join("notes.md");
    let out = run(&f, &["--skip-tests", "--notes", notes.to_str().expect("utf8")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&notes).expect("notes"), "- Only, see [docs](https://x).\n");
}

#[test]
fn rc_tag_labels_packages_manifest_urls_and_notes() {
    let f = fixture("rc", CHANGELOG_OK);
    let notes = f.base.0.join("notes.md");
    let out = run(&f, &["--skip-tests", "--tag", "v0.4.2-rc.3", "--notes", notes.to_str().expect("utf8")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    let dir = f.repo.join("dist-release/v0.4.2-rc.3");
    let m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("latest.json")).expect("latest.json")).expect("json");
    assert_eq!(m["version"], "0.4.2-rc.3");
    for name in ["x86_64-linux", "aarch64-linux"] {
        let file = format!("nants-0.4.2-rc.3-{name}.tar.gz");
        assert_eq!(
            m["assets"][name]["url"],
            format!("https://github.com/tm-dev-lab/nants/releases/download/v0.4.2-rc.3/{file}"),
            "{m}"
        );
        let sum_line = std::fs::read_to_string(dir.join(format!("{file}.sha256"))).expect("sha256");
        assert_eq!(m["assets"][name]["sha256"], sum_line.split_whitespace().next().unwrap_or(""));
        assert!(sum_line.trim_end().ends_with(&file), "{sum_line}");
    }
    assert!(!f.repo.join("dist-release/v0.4.2").exists(), "the rc does not write the final folder");
    assert_eq!(
        std::fs::read_to_string(&notes).expect("notes"),
        "**Test pre-release 0.4.2-rc.3.** It is not the final 0.4.2 release.\n\nFirst line.\n\n### Added\n\n- Something.\n- Another `thing`.\n"
    );
    assert_no_publishing(&calls(&f));

    // The explicit final tag is the same as no tag.
    let out = run(&f, &["--skip-tests", "--tag", "v0.4.2"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(manifest(&f)["version"], "0.4.2");
}

#[test]
fn refuses_tags_with_another_format_or_another_version() {
    let f = fixture("badtag", CHANGELOG_OK);
    for (tag, reason) in [
        ("v0.5.0", "does not match the Cargo.toml version 0.4.2"),
        ("v0.4.1-rc.1", "does not match the Cargo.toml version 0.4.2"),
        ("v0.4.2-beta", "invalid tag"),
        ("v0.4.2-rc", "invalid tag"),
        ("v0.4.2-rc.1.2", "invalid tag"),
        ("0.4.2", "invalid tag"),
        ("v0.4.2\nv0.4.2", "invalid tag"),
    ] {
        let out = run(&f, &["--skip-tests", "--tag", tag], &[]);
        assert!(!out.status.success(), "{tag:?}");
        assert!(text(&out).contains(reason), "{tag:?}: {}", text(&out));
    }
    assert!(!calls(&f).contains("cargo"), "refuses before building");
    assert!(!f.repo.join("dist-release").exists());
}

/// The user's examples: Cargo.toml in 0.1.0 refuses v0.2.0 and v0.1.0-beta.
#[test]
fn cargo_0_1_0_refuses_v0_2_0_and_v0_1_0_beta() {
    let f = fixture("v010", "# Changelog\n\n## [0.1.0] - 2026-10-22\n\n- First.\n");
    let cargo = "[package]\nname = \"nants\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = { version = \"1\" }\n";
    write(&f.repo.join("Cargo.toml"), cargo);
    assert!(git(&f.repo, &["commit", "-q", "-am", "0.1.0"]).status.success());
    for tag in ["v0.2.0", "v0.1.0-beta"] {
        let out = run(&f, &["--skip-tests", "--tag", tag], &[]);
        assert!(!out.status.success(), "{tag}: {}", text(&out));
    }
    let out = run(&f, &["--skip-tests", "--tag", "v0.1.0-rc.1"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(f.repo.join("dist-release/v0.1.0-rc.1/nants-0.1.0-rc.1-x86_64-linux.tar.gz").exists());
}

#[test]
fn refuses_changelog_without_the_version_section() {
    let f = fixture("changelog", "# Changelog\n\n## [0.4.1] - 2026-01-01\n\n### Added\n- Before.\n\n[0.4.2]: x\n");
    let out = run(&f, &[], &[]);
    assert!(!out.status.success());
    assert!(text(&out).contains("no ## [0.4.2] section"), "{}", text(&out));
    assert!(!calls(&f).contains("cargo"), "refuses before testing or building");
    assert!(!f.repo.join("dist-release").exists());
}

#[test]
fn refuses_dirty_tree_and_lists_the_paths() {
    let f = fixture("dirty", CHANGELOG_OK);
    write(&f.repo.join("loose.txt"), "x\n");
    write(&f.repo.join("CHANGELOG.md"), &format!("{CHANGELOG_OK}- Changed.\n"));
    let out = run(&f, &[], &[]);
    assert!(!out.status.success());
    let all = text(&out);
    assert!(all.contains("uncommitted changes"), "{all}");
    assert!(all.contains("pending paths (git status --porcelain, 2 in total)"), "{all}");
    assert!(all.contains("?? loose.txt"), "{all}");
    assert!(all.contains(" M CHANGELOG.md"), "{all}");
    assert!(!all.contains("Changed."), "no file contents: {all}");
    assert!(!calls(&f).contains("cargo"), "refuses before testing or building");
}

#[test]
fn dirty_tree_listing_stops_at_thirty_paths() {
    let f = fixture("dirty-many", CHANGELOG_OK);
    for i in 0..35 {
        write(&f.repo.join(format!("loose-{i:02}.txt")), "x\n");
    }
    let out = run(&f, &["--skip-tests"], &[]);
    assert!(!out.status.success());
    let all = text(&out);
    assert_eq!(all.lines().filter(|l| l.contains("?? loose-")).count(), 30, "{all}");
    assert!(all.contains("35 in total") && all.contains("and 5 more"), "{all}");
}

#[test]
fn refuses_when_the_interface_build_dirties_the_tree() {
    let f = fixture("dirty-build", CHANGELOG_OK);
    script(
        &f.bin,
        "npm",
        &FAKE_NPM.replace("exit 0", "[ \"$1 $2\" = \"run build\" ] && mkdir -p dist && echo x > dist/new.js\nexit 0"),
    );
    let out = run(&f, &[], &[]);
    assert!(!out.status.success());
    let all = text(&out);
    assert!(all.contains("the interface build"), "{all}");
    assert!(all.contains("?? web/dist/"), "{all}");
    assert!(!calls(&f).contains("zigbuild"), "no build with a dirty tree");
}

#[test]
fn refuses_failing_tests_before_building() {
    let f = fixture("tests", CHANGELOG_OK);
    let notes = f.base.0.join("notes.md");
    let out = run(&f, &["--notes", notes.to_str().expect("utf8")], &[("FAKE_TEST_FAIL", "1")]);
    assert!(!out.status.success());
    assert!(text(&out).contains("cargo test failed"), "{}", text(&out));
    let log = calls(&f);
    assert!(!log.contains("zigbuild"), "no build with a failing test: {log}");
    assert!(!notes.exists(), "no notes on failure");
    assert!(!f.repo.join("dist-release/v0.4.2/latest.json").exists());
}

#[test]
fn skip_tests_runs_no_tests() {
    let f = fixture("skip", CHANGELOG_OK);
    let out = run(&f, &["--skip-tests"], &[("FAKE_TEST_FAIL", "1")]);
    assert!(out.status.success(), "{}", text(&out));
    let log = calls(&f);
    assert!(!log.contains("cargo test") && !log.contains("npm "), "{log}");
    assert!(!log.contains("tmpdir "), "{log}");
    assert!(log.contains("cargo zigbuild"), "{log}");
    assert_no_publishing(&log);
}

#[test]
fn unknown_options_and_missing_values_are_refused() {
    let f = fixture("args", CHANGELOG_OK);
    for args in [&["--dry-run"][..], &["--notes"][..], &["--base-url"][..], &["--tag"][..]] {
        let out = run(&f, args, &[]);
        assert!(!out.status.success(), "{args:?}");
    }
    assert!(!calls(&f).contains("cargo"));
}

#[test]
fn runs_under_dash() {
    if !Path::new("/usr/bin/dash").exists() && !Path::new("/bin/dash").exists() {
        return;
    }
    let f = fixture("dash", CHANGELOG_OK);
    let notes = f.base.0.join("notes.md");
    let out = run_with("dash", &f, &["--notes", notes.to_str().expect("utf8")], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(manifest(&f)["assets"].as_object().map(|a| a.len()), Some(2));
    assert!(std::fs::read_to_string(&notes).expect("notes").starts_with("First line."));
}

#[test]
fn script_is_posix_and_english() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/package.sh");
    let body = std::fs::read_to_string(&path).expect("package.sh");
    assert!(body.starts_with("#!/bin/sh\n"));
    assert!(body.contains("\nset -eu\n"));
    assert!(!body.contains('\u{2014}') && !body.contains('\u{2013}'), "no dashes");
    assert!(body.is_ascii(), "public script in English, plain ASCII");
    for word in ["az ", "git tag", "git push", "gh release"] {
        assert!(!body.contains(word), "{word}");
    }
    for sh in ["dash", "sh"] {
        let Ok(out) = Command::new(sh).arg("-n").arg(&path).output() else { continue };
        assert!(out.status.success(), "{sh} -n: {}", String::from_utf8_lossy(&out.stderr));
    }
}
