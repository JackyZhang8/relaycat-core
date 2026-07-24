use relaycat_protocol::WorkspaceErrorCode;
use relaycat_workspace::{ProjectRoot, ShellManager};
use std::{fs, path::PathBuf, thread, time::{Duration, SystemTime, UNIX_EPOCH}};

struct Fixture { root: PathBuf }
impl Fixture { fn new()->Self{let n=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();let root=std::env::temp_dir().join(format!("relaycat-shell-{n}"));fs::create_dir_all(&root).unwrap();Self{root}} }
impl Drop for Fixture { fn drop(&mut self){let _=fs::remove_dir_all(&self.root);} }

#[test]
fn shell_limit_input_dedupe_and_snapshot_replay() {
    let fixture = Fixture::new();
    let manager = ShellManager::new(ProjectRoot::open(&fixture.root).unwrap(), 3);
    let first = manager.create(80, 24).unwrap();
    manager.input(&first.descriptor.shell_id, 1, b"printf replay-marker\\n".to_vec()).unwrap();
    manager.input(&first.descriptor.shell_id, 1, b"printf duplicate\\n".to_vec()).unwrap();
    thread::sleep(Duration::from_millis(250));
    let snapshot = manager.snapshot(&first.descriptor.shell_id, 0).unwrap();
    let output = String::from_utf8_lossy(&snapshot.bytes);
    assert!(output.contains("replay-marker"));
    assert!(!output.contains("duplicate"));
    manager.create(80, 24).unwrap();
    manager.create(80, 24).unwrap();
    assert_eq!(manager.create(80, 24).unwrap_err().code(), WorkspaceErrorCode::Busy);
    manager.close_all().unwrap();
}
