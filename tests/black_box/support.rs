//! A scratch directory per test, and what the tests do in it. Paths are plain strings,
//! because every one of them is put together here and most end up as arguments.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const GITCONFIG: &str = "\
[user]
\tname = test
\temail = test@example.invalid
[init]
\tdefaultBranch = main
[protocol \"file\"]
\tallow = always
[advice]
\tdetachedHead = false
# Background maintenance after a commit adds and removes files under objects/, which the
# tests that prove a command writes nothing would then blame on that command.
[maintenance]
\tauto = false
# The machine this ships to sets it globally; the tests must hold under it.
[submodule]
\trecurse = true
";

/// What the caller's environment could use to steer git, direnv or the tool itself.
const INHERITED_STEERING: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "WM_WORKTREE_PATH",
    "DIRENV_DIR",
    "DIRENV_FILE",
    "DIRENV_DIFF",
    "DIRENV_WATCHES",
];

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Sandbox {
    /// Holds this test's home and configuration, and is where the tool runs by default:
    /// never the caller's directory, because `sweep` also sweeps the repository it runs in.
    pub root: String,
    /// Holds the repositories.
    pub t: String,
    /// The main checkout, once a project has been made.
    pub main: String,
}

pub struct Worktree {
    pub path: String,
    pub gitdir: String,
}

impl Worktree {
    pub fn file(&self, name: &str) -> String {
        format!("{}/{name}", self.path)
    }

    pub fn git_file(&self, name: &str) -> String {
        format!("{}/{name}", self.gitdir)
    }
}

pub struct Ran {
    pub rc: i32,
    pub out: String,
    pub err: String,
}

impl Ran {
    fn from_output(output: &Output) -> Self {
        Self {
            // A signal has no code, and is never the code a test expects.
            rc: output.status.code().unwrap_or(-1),
            out: text(&output.stdout),
            err: text(&output.stderr),
        }
    }

    #[track_caller]
    pub fn rc(&self, expected: i32) {
        assert_eq!(
            self.rc, expected,
            "unexpected exit status; stdout: {}; stderr: {}",
            self.out, self.err
        );
    }

    #[track_caller]
    pub fn out_has(&self, wanted: &str) {
        assert!(
            self.out.contains(wanted),
            "stdout lacks '{wanted}': {}",
            self.out
        );
    }

    #[track_caller]
    pub fn err_has(&self, wanted: &str) {
        assert!(
            self.err.contains(wanted),
            "stderr lacks '{wanted}': {}",
            self.err
        );
    }
}

/// A process sitting in a directory, stopped however the test ends.
pub struct Holder(Child);

impl Holder {
    pub fn pid(&self) -> u32 {
        self.0.id()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        // Already gone is fine: all that matters is that it does not outlive the test.
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

impl Sandbox {
    pub fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wk-black-box-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir).expect("the scratch directory should be created");
        let root = fs::canonicalize(&dir)
            .expect("the scratch directory should resolve")
            .to_str()
            .expect("the temporary directory should be UTF-8")
            .to_owned();
        let t = format!("{root}/t");
        let main = format!("{t}/project");
        mkdir(&t);
        mkdir(format!("{root}/home"));
        write(format!("{root}/gitconfig"), GITCONFIG);
        Self { root, t, main }
    }

    pub fn with_project() -> Self {
        Self::with_project_at("sub")
    }

    /// A project with a submodule that itself has a nested submodule, all pushed to local
    /// bare remotes, checked out on main. A path with a slash is still a top-level submodule.
    pub fn with_project_at(subpath: &str) -> Self {
        let s = Self::new();
        let t = &s.t;
        for repo in ["nested", "sub", "project"] {
            let seed = format!("{t}/{repo}-seed");
            s.git(t, &["init", "-q", "--bare", &format!("{repo}.git")]);
            s.git(t, &["clone", "-q", &format!("{repo}.git"), &seed]);
            write(format!("{seed}/README"), &format!("{repo}\n"));
            s.git(&seed, &["add", "README"]);
            s.git(&seed, &["commit", "-q", "-m", &format!("init {repo}")]);
            s.git(&seed, &["push", "-q", "origin", "main"]);
        }
        s.add_submodule(
            &format!("{t}/sub-seed"),
            &format!("{t}/nested.git"),
            "nested",
        );
        s.add_submodule(
            &format!("{t}/project-seed"),
            &format!("{t}/sub.git"),
            subpath,
        );
        s.git(
            t,
            &[
                "clone",
                "-q",
                "--recurse-submodules",
                "project.git",
                "project",
            ],
        );
        s
    }

    fn add_submodule(&self, seed: &str, remote: &str, path: &str) {
        self.git(seed, &["submodule", "-q", "add", remote, path]);
        self.git(seed, &["commit", "-q", "-m", &format!("add {path}")]);
        self.git(seed, &["push", "-q", "origin", "main"]);
    }

    /// A linked worktree of the project, with its submodules initialised the way
    /// `worktree provision` initialises them.
    pub fn worktree(&self, branch: &str) -> Worktree {
        let path = format!("{}/wt-{branch}", self.t);
        self.git(&self.main, &["worktree", "add", "-q", "-b", branch, &path]);
        self.git(
            &path,
            &["submodule", "-q", "update", "--init", "--recursive"],
        );
        let gitdir = self.git(&path, &["rev-parse", "--absolute-git-dir"]);
        Worktree { path, gitdir }
    }

    /// A command that sees nothing of the machine's git, direnv or home.
    pub fn command(&self, program: &str) -> Command {
        let root = &self.root;
        let mut command = Command::new(program);
        for name in INHERITED_STEERING {
            command.env_remove(name);
        }
        command
            .env("HOME", format!("{root}/home"))
            .env("XDG_DATA_HOME", format!("{root}/xdg-data"))
            .env("XDG_CONFIG_HOME", format!("{root}/xdg-config"))
            .env("XDG_CACHE_HOME", format!("{root}/xdg-cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", format!("{root}/gitconfig"))
            .stdin(Stdio::null())
            .current_dir(root);
        command
    }

    /// The tool under test, not yet run.
    pub fn tool(&self) -> Command {
        self.command(&binary())
    }

    pub fn run(&self, args: &[&str]) -> Ran {
        self.run_in(&self.root, args)
    }

    pub fn run_in(&self, dir: &str, args: &[&str]) -> Ran {
        let mut command = self.tool();
        command.current_dir(dir).args(args);
        ran(&mut command)
    }

    /// Creates a worktree from the main checkout, and returns the path it printed.
    #[track_caller]
    pub fn create(&self, branch: &str) -> String {
        let created = self.run_in(&self.main, &["create", branch]);
        created.rc(0);
        created.out
    }

    /// Setup, so failing it fails the test.
    #[track_caller]
    pub fn git(&self, dir: &str, args: &[&str]) -> String {
        let output = self
            .command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} in {dir}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        text(&output.stdout)
    }

    pub fn git_ok(&self, dir: &str, args: &[&str]) -> bool {
        self.command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start")
            .status
            .success()
    }

    pub fn has_branch(&self, repo: &str, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        self.git_ok(repo, &["show-ref", "--verify", "--quiet", &reference])
    }

    /// A commit on a new file, in a worktree or the main checkout.
    pub fn commit_file(&self, dir: &str, name: &str, content: &str) {
        write(format!("{dir}/{name}"), &format!("{content}\n"));
        self.git(dir, &["add", name]);
        self.git(dir, &["commit", "-q", "-m", name]);
    }

    /// An edit committed on a new branch in the worktree's submodule: the only copy.
    pub fn commit_in_submodule(&self, wt: &Worktree) {
        let sub = wt.file("sub");
        self.git(&sub, &["checkout", "-q", "-b", "work"]);
        append(wt.file("sub/README"), "change\n");
        self.git(&sub, &["commit", "-q", "-am", "unpushed in sub"]);
    }

    /// A process whose current directory is `dir`.
    pub fn holder_in(&self, dir: &str) -> Holder {
        let child = self
            .command("sleep")
            .arg("300")
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep should start");
        Holder(child)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A test that failed may have left a directory read-only.
        open_up(Path::new(&self.root));
        // Scratch left behind is not worth failing a test over.
        drop(fs::remove_dir_all(&self.root));
    }
}

fn open_up(dir: &Path) {
    drop(fs::set_permissions(dir, fs::Permissions::from_mode(0o755)));
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            open_up(&entry.path());
        }
    }
}

fn binary() -> String {
    std::env::var("WORKTREE_UNDER_TEST")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_worktree").to_owned())
}

/// Output as a shell's `$(...)` gives it: without the final newlines.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\n')
        .to_owned()
}

pub fn ran(command: &mut Command) -> Ran {
    Ran::from_output(
        &command
            .output()
            .expect("the command under test should start"),
    )
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_dir()
}

pub fn is_file(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}

pub fn is_symlink(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_symlink()
}

#[track_caller]
pub fn mkdir(path: impl AsRef<Path>) {
    fs::create_dir_all(&path).expect("the directory should be created");
}

#[track_caller]
pub fn write(path: impl AsRef<Path>, content: &str) {
    fs::write(&path, content).expect("the file should be written");
}

#[track_caller]
pub fn append(path: impl AsRef<Path>, content: &str) {
    let existing = fs::read_to_string(&path).expect("the file should be readable");
    write(path, &format!("{existing}{content}"));
}

/// A file's content without its final newlines.
#[track_caller]
pub fn read(path: impl AsRef<Path>) -> String {
    text(&fs::read(&path).expect("the file should be readable"))
}

#[track_caller]
pub fn rm(path: impl AsRef<Path>) {
    fs::remove_file(&path).expect("the file should be removed");
}

#[track_caller]
pub fn rm_rf(path: impl AsRef<Path>) {
    fs::remove_dir_all(&path).expect("the directory should be removed");
}

#[track_caller]
pub fn chmod(path: impl AsRef<Path>, mode: u32) {
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("the mode should change");
}

#[track_caller]
pub fn mode_of(path: impl AsRef<Path>) -> u32 {
    fs::metadata(&path)
        .expect("the file should exist")
        .permissions()
        .mode()
        & 0o777
}

/// Root reads through mode 000 and writes through 555, so those tests hold only for others.
pub fn running_as_root(sandbox: &Sandbox) -> bool {
    fs::metadata(&sandbox.root)
        .expect("the scratch directory should exist")
        .uid()
        == 0
}

/// Whether each piece appears, in this order.
pub fn in_order(haystack: &str, pieces: &[&str]) -> bool {
    let mut rest = haystack;
    pieces.iter().all(|piece| match rest.find(piece) {
        Some(at) => {
            rest = &rest[at + piece.len()..];
            true
        }
        None => false,
    })
}

/// Every path under the directories, sorted, with `describe` applied to what is there.
fn walk(dir: &Path, found: &mut Vec<String>, describe: &impl Fn(&Path, &fs::Metadata) -> String) {
    let Ok(metadata) = fs::symlink_metadata(dir) else {
        return;
    };
    found.push(describe(dir, &metadata));
    if metadata.is_dir() {
        for entry in fs::read_dir(dir)
            .expect("the directory should be readable")
            .flatten()
        {
            walk(&entry.path(), found, describe);
        }
    }
}

/// Everything a question could touch, by path, mtime, size and mode.
pub fn tree_state(dirs: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for dir in dirs {
        walk(Path::new(dir), &mut found, &|path, metadata| {
            format!(
                "{} {}.{} {} {:o}",
                path.display(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.size(),
                metadata.mode()
            )
        });
    }
    found.sort();
    found
}

/// Every path under a directory, sorted.
pub fn paths_under(dir: &str) -> Vec<String> {
    let mut found = Vec::new();
    walk(Path::new(dir), &mut found, &|path, _| {
        path.display().to_string()
    });
    found.sort();
    found
}
