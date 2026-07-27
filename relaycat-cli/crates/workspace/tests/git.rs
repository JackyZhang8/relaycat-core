use relaycat_protocol::GitDiffTarget;
use relaycat_workspace::{GitService, ProjectRoot};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static REPO_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Repo { root: PathBuf }
impl Repo {
    fn new() -> Self {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let sequence = REPO_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "relaycat-workspace-git-{}-{suffix}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "--quiet"]);
        run(&root, &["config", "user.name", "RelayCat Test"]);
        run(&root, &["config", "user.email", "relaycat@example.test"]);
        for index in 0..25 {
            fs::write(root.join("history.txt"), format!("{index}\n")).unwrap();
            run(&root, &["add", "--", "history.txt"]);
            run(&root, &["commit", "--quiet", "-m", &format!("commit {index}")]);
        }
        Self { root }
    }
}
impl Drop for Repo { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
fn run(root: &PathBuf, args: &[&str]) { assert!(Command::new("git").args(args).current_dir(root).status().unwrap().success()); }

#[test]
fn status_history_and_diff_are_stable_and_paged() {
    let repo = Repo::new();
    fs::write(repo.root.join("history.txt"), "changed\n").unwrap();
    let git = GitService::new(ProjectRoot::open(&repo.root).unwrap());
    let first = git.status().unwrap();
    assert_eq!(first.fingerprint, git.status().unwrap().fingerprint);
    assert_eq!(first.changes.len(), 1);
    let page = git.history("", "", "", None, 20).unwrap();
    assert_eq!(page.items.len(), 20);
    assert!(page.next_cursor.is_some());
    assert!(!git.diff(GitDiffTarget::WorkingTree, None, 768 * 1024).unwrap().bytes.is_empty());
}

#[test]
fn path_operations_accept_leading_dash_safely() {
    let repo = Repo::new();
    fs::write(repo.root.join("-danger.txt"), "safe\n").unwrap();
    let git = GitService::new(ProjectRoot::open(&repo.root).unwrap());
    git.stage(&["-danger.txt".to_string()]).unwrap();
    let staged = Command::new("git").args(["diff", "--cached", "--name-only"])
        .current_dir(&repo.root).output().unwrap();
    assert!(String::from_utf8_lossy(&staged.stdout).contains("-danger.txt"));
}

#[test]
fn staged_rename_is_one_change_with_original_path() {
    let repo = Repo::new();
    fs::rename(repo.root.join("history.txt"), repo.root.join("renamed.txt")).unwrap();
    run(&repo.root, &["add", "-A"]);
    let git = GitService::new(ProjectRoot::open(&repo.root).unwrap());

    let status = git.status().unwrap();

    assert_eq!(status.changes.len(), 1);
    assert_eq!(status.changes[0].path, "renamed.txt");
    assert_eq!(status.changes[0].old_path.as_deref(), Some("history.txt"));
    assert_eq!(status.changes[0].status, "renamed");
    assert!(status.changes[0].staged);
}

#[test]
fn subdirectory_project_status_is_scoped_and_project_relative() {
    let repo = Repo::new();
    let project = repo.root.join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("inside.txt"), "initial\n").unwrap();
    fs::write(repo.root.join("outside.txt"), "initial\n").unwrap();
    run(&repo.root, &["add", "--", "project/inside.txt", "outside.txt"]);
    run(&repo.root, &["commit", "--quiet", "-m", "add scoped files"]);
    fs::write(project.join("inside.txt"), "changed\n").unwrap();
    fs::write(repo.root.join("outside.txt"), "changed\n").unwrap();
    let git = GitService::new(ProjectRoot::open(&project).unwrap());

    let status = git.status().unwrap();

    assert_eq!(status.changes.iter().map(|change| change.path.as_str()).collect::<Vec<_>>(), vec!["inside.txt"]);
    git.stage(&["inside.txt".to_string()]).unwrap();
    let staged = Command::new("git").args(["diff", "--cached", "--name-only"])
        .current_dir(&repo.root).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "project/inside.txt");
}
