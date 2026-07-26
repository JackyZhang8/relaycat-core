use relaycat_protocol::WorkspaceErrorCode;
use relaycat_workspace::{ProjectRoot, ShellManager, ShellTransportEvent};
use std::{fs, path::PathBuf, thread, time::{Duration, SystemTime, UNIX_EPOCH}};

struct Fixture { root: PathBuf }
impl Fixture { fn new()->Self{let n=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();let root=std::env::temp_dir().join(format!("relaycat-shell-{n}"));fs::create_dir_all(&root).unwrap();Self{root}} }
impl Drop for Fixture { fn drop(&mut self){let _=fs::remove_dir_all(&self.root);} }

#[test]
fn shell_terminal_stream_enforces_one_shell_and_drains_pty_output() {
    let fixture = Fixture::new();
    let manager = ShellManager::new(ProjectRoot::open(&fixture.root).unwrap(), 3);
    let created = manager.create(80, 24).unwrap();

    assert_eq!(
        manager.create(80, 24).unwrap_err().code(),
        WorkspaceErrorCode::Busy
    );
    assert_eq!(manager.active_shell_id().as_deref(), Some(created.descriptor.shell_id.as_str()));

    manager
        .write_active(b"printf transport-marker\n".to_vec())
        .unwrap();
    thread::sleep(Duration::from_millis(250));

    let events = manager.drain_transport_events();
    assert!(events.iter().any(|event| matches!(
        event,
        ShellTransportEvent::Started(descriptor)
            if descriptor.shell_id == created.descriptor.shell_id
    )));
    let output: Vec<u8> = events
        .into_iter()
        .filter_map(|event| match event {
            ShellTransportEvent::Output { bytes, .. } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(String::from_utf8_lossy(&output).contains("transport-marker"));

    manager.resize_active(101, 33).unwrap();
    let descriptor = manager.list().into_iter().next().unwrap();
    assert_eq!((descriptor.cols, descriptor.rows), (101, 33));
    manager.close_all().unwrap();
}

#[test]
fn shell_terminal_stream_reports_close_after_shell_is_removed() {
    let fixture = Fixture::new();
    let manager = ShellManager::new(ProjectRoot::open(&fixture.root).unwrap(), 1);
    let created = manager.create(80, 24).unwrap();
    let _ = manager.drain_transport_events();

    manager.close(&created.descriptor.shell_id).unwrap();

    assert!(manager.drain_transport_events().iter().any(|event| matches!(
        event,
        ShellTransportEvent::Exit { shell_id, .. }
            if shell_id == &created.descriptor.shell_id
    )));
}
