use relaycat_protocol::WorkspaceErrorCode;
use relaycat_workspace::ProjectRoot;
use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

struct Fixture { root: PathBuf }

impl Fixture {
    fn new() -> Self {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("relaycat-workspace-security-{suffix}"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}

#[test]
fn rejects_parent_and_absolute_paths() {
    let fixture = Fixture::new();
    let root = ProjectRoot::open(&fixture.root).unwrap();
    assert_eq!(root.resolve("../secret").unwrap_err().code(), WorkspaceErrorCode::PathOutsideProject);
    assert_eq!(root.resolve("/etc/passwd").unwrap_err().code(), WorkspaceErrorCode::PathOutsideProject);
    assert_eq!(root.resolve("src/lib.rs").unwrap(), fixture.root.join("src/lib.rs").canonicalize().unwrap());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = fixture.root.with_extension("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), "nope").unwrap();
    symlink(&outside, fixture.root.join("escape")).unwrap();
    let root = ProjectRoot::open(&fixture.root).unwrap();
    assert_eq!(root.resolve("escape/secret").unwrap_err().code(), WorkspaceErrorCode::PathOutsideProject);
    fs::remove_dir_all(outside).unwrap();
}
