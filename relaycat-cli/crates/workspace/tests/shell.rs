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

#[test]
fn naturally_exited_shell_is_reaped_before_the_next_create() {
    let fixture = Fixture::new();
    let manager = ShellManager::new(ProjectRoot::open(&fixture.root).unwrap(), 1);
    let first = manager.create(80, 24).unwrap();
    let _ = manager.drain_transport_events();

    manager.write_active(b"exit\n".to_vec()).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_exit = false;
    while std::time::Instant::now() < deadline {
        if manager.drain_transport_events().iter().any(|event| {
            matches!(
                event,
                ShellTransportEvent::Exit { shell_id, .. }
                    if shell_id == &first.descriptor.shell_id
            )
        }) {
            saw_exit = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    assert!(saw_exit, "the naturally exited PTY should publish Exit");
    assert!(
        manager.list().is_empty(),
        "the exited Shell must not remain reusable"
    );

    let second = manager.create(80, 24).unwrap();
    assert_ne!(second.descriptor.shell_id, first.descriptor.shell_id);
    manager.close_all().unwrap();
}

#[test]
fn replacement_created_before_drain_preserves_final_output_and_exit() {
    let fixture = Fixture::new();
    let manager = ShellManager::new(ProjectRoot::open(&fixture.root).unwrap(), 1);
    let first = manager.create(80, 24).unwrap();
    let _ = manager.drain_transport_events();

    manager
        .write_active(b"printf 'final-marker\\n'; exit\n".to_vec())
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !manager.list().is_empty() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        manager.list().is_empty(),
        "the exited Shell should stop blocking create"
    );

    let second = manager.create(80, 24).unwrap();
    let events = manager.drain_transport_events();
    let old_output: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            ShellTransportEvent::Output { shell_id, bytes }
                if shell_id == &first.descriptor.shell_id =>
            {
                Some(bytes.as_slice())
            }
            _ => None,
        })
        .flatten()
        .copied()
        .collect();

    assert!(String::from_utf8_lossy(&old_output).contains("final-marker"));
    assert!(events.iter().any(|event| matches!(
        event,
        ShellTransportEvent::Exit { shell_id, .. }
            if shell_id == &first.descriptor.shell_id
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ShellTransportEvent::Started(descriptor)
            if descriptor.shell_id == second.descriptor.shell_id
    )));
    let old_exit_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ShellTransportEvent::Exit { shell_id, .. }
                    if shell_id == &first.descriptor.shell_id
            )
        })
        .unwrap();
    let new_start_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ShellTransportEvent::Started(descriptor)
                    if descriptor.shell_id == second.descriptor.shell_id
            )
        })
        .unwrap();
    assert!(
        old_exit_index < new_start_index,
        "old Exit must precede replacement Started"
    );
    manager.close_all().unwrap();
}
