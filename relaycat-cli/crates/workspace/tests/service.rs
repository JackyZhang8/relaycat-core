use relaycat_protocol::{GitRemoteOperation, PlainMsg, WorkspaceEvent, WorkspaceRequest, WorkspaceRequestEnvelope, WorkspaceResponse};
use relaycat_workspace::{ProjectRoot, ShellTransportEvent, WorkspaceService};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_REPO_ID: AtomicU64 = AtomicU64::new(1);

struct Repo { root: PathBuf }
impl Repo { fn new()->Self{let n=NEXT_REPO_ID.fetch_add(1,Ordering::Relaxed);let root=std::env::temp_dir().join(format!("relaycat-service-{}-{n}",std::process::id()));fs::create_dir_all(&root).unwrap();git(&root,&["init","--quiet"]);git(&root,&["config","user.name","RelayCat Test"]);git(&root,&["config","user.email","relaycat@example.test"]);fs::write(root.join("a.txt"),"a").unwrap();git(&root,&["add","--","a.txt"]);git(&root,&["commit","--quiet","-m","initial"]);Self{root}} }
impl Drop for Repo{fn drop(&mut self){let _=fs::remove_dir_all(&self.root);}}
fn git(root:&PathBuf,args:&[&str]){assert!(Command::new("git").args(args).current_dir(root).status().unwrap().success());}

#[test]
fn repeated_idempotency_key_returns_cached_commit_result() {
    let repo=Repo::new(); fs::write(repo.root.join("b.txt"),"b").unwrap(); git(&repo.root,&["add","--","b.txt"]);
    let root=ProjectRoot::open(&repo.root).unwrap(); let project_id=root.project_id().to_string(); let service=WorkspaceService::new(root,3);
    let request=WorkspaceRequestEnvelope{request_id:"req-1".into(),project_id:project_id.clone(),deadline_unix_ms:u64::MAX,idempotency_key:Some("idem-1".into()),operation:WorkspaceRequest::GitCommit{message:"second".into(),amend:false,push_after:false}};
    let first=service.execute(request.clone());
    let second=service.execute(WorkspaceRequestEnvelope{request_id:"req-2".into(),..request});
    assert_eq!(first.result,second.result);
    assert_eq!(Command::new("git").args(["rev-list","--count","HEAD"]).current_dir(&repo.root).output().unwrap().stdout,b"2\n");
    assert!(matches!(first.result,Ok(WorkspaceResponse::GitMutation(_))));
}

#[test]
fn rejects_wrong_project_id() {
    let repo=Repo::new(); let root=ProjectRoot::open(&repo.root).unwrap(); let service=WorkspaceService::new(root,3);
    let response=service.execute(WorkspaceRequestEnvelope{request_id:"bad".into(),project_id:"other".into(),deadline_unix_ms:u64::MAX,idempotency_key:None,operation:WorkspaceRequest::GitStatus});
    assert!(response.result.is_err());
    let _ = PlainMsg::WorkspaceResponse(response);
}

#[test]
fn workspace_service_exposes_shell_transport_to_cli_host() {
    let repo = Repo::new();
    let root = ProjectRoot::open(&repo.root).unwrap();
    let project_id = root.project_id().to_string();
    let service = WorkspaceService::new(root, 3);
    let response = service.execute(WorkspaceRequestEnvelope {
        request_id: "shell-create".into(),
        project_id,
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::ShellCreate { cols: 80, rows: 24 },
    });
    assert!(matches!(response.result, Ok(WorkspaceResponse::ShellCreated(_))));

    service
        .write_workspace_shell(b"printf service-marker\n".to_vec())
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let events = service.drain_shell_transport_events();
    assert!(events.iter().any(|event| matches!(event, ShellTransportEvent::Started(_))));
    assert!(events.iter().any(|event| matches!(
        event,
        ShellTransportEvent::Output { bytes, .. }
            if String::from_utf8_lossy(&bytes).contains("service-marker")
    )));
    assert!(service.drain_events().is_empty(), "raw Shell output must not enter workspace events");

    service.resize_workspace_shell(90, 30).unwrap();
    assert_eq!(service.shell_descriptors()[0].cols, 90);
}

#[test]
fn workspace_shell_create_is_idempotent_across_gui_and_app_races() {
    let repo = Repo::new();
    let root = ProjectRoot::open(&repo.root).unwrap();
    let project_id = root.project_id().to_string();
    let service = WorkspaceService::new(root, 1);
    let first = service.execute(WorkspaceRequestEnvelope {
        request_id: "first-create".into(),
        project_id: project_id.clone(),
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::ShellCreate { cols: 80, rows: 24 },
    });
    let second = service.execute(WorkspaceRequestEnvelope {
        request_id: "second-create".into(),
        project_id,
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::ShellCreate { cols: 100, rows: 30 },
    });
    let shell_id = match first.result.unwrap() {
        WorkspaceResponse::ShellCreated(snapshot) => snapshot.descriptor.shell_id,
        other => panic!("unexpected response: {other:?}"),
    };
    match second.result.unwrap() {
        WorkspaceResponse::ShellCreated(snapshot) => assert_eq!(snapshot.descriptor.shell_id, shell_id),
        other => panic!("unexpected response: {other:?}"),
    }
    assert_eq!(service.shell_descriptors().len(), 1);
}

#[test]
fn raw_shell_sync_rpc_is_rejected_in_favor_of_terminal_stream_v2() {
    let repo = Repo::new();
    let root = ProjectRoot::open(&repo.root).unwrap();
    let project_id = root.project_id().to_string();
    let service = WorkspaceService::new(root, 1);
    let create = service.execute(WorkspaceRequestEnvelope {
        request_id: "create".into(),
        project_id: project_id.clone(),
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::ShellCreate { cols: 80, rows: 24 },
    });
    let shell_id = match create.result.unwrap() {
        WorkspaceResponse::ShellCreated(snapshot) => snapshot.descriptor.shell_id,
        other => panic!("unexpected response: {other:?}"),
    };

    for operation in [
        WorkspaceRequest::ShellInput {
            shell_id: shell_id.clone(),
            input_seq: 1,
            bytes: b"pwd\r".to_vec(),
        },
        WorkspaceRequest::ShellResize {
            shell_id: shell_id.clone(),
            cols: 90,
            rows: 30,
        },
        WorkspaceRequest::ShellSnapshot {
            shell_id: shell_id.clone(),
            after_output_seq: 0,
        },
    ] {
        let response = service.execute(WorkspaceRequestEnvelope {
            request_id: "legacy-control".into(),
            project_id: project_id.clone(),
            deadline_unix_ms: u64::MAX,
            idempotency_key: None,
            operation,
        });
        assert_eq!(
            response.result.unwrap_err().code,
            relaycat_protocol::WorkspaceErrorCode::Unsupported
        );
    }
}

#[test]
fn failed_git_remote_progress_contains_the_response_error_detail() {
    let repo = Repo::new();
    git(&repo.root, &["remote", "add", "origin", "/relaycat-missing-remote"]);
    let root = ProjectRoot::open(&repo.root).unwrap();
    let project_id = root.project_id().to_string();
    let service = WorkspaceService::new(root, 1);
    let response = service.execute(WorkspaceRequestEnvelope {
        request_id: "fetch-failure".into(),
        project_id,
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::GitRemote { kind: GitRemoteOperation::Fetch },
    });
    let error = response.result.unwrap_err();
    assert!(error.message.contains("fatal:"));
    let completed_message = service.drain_events().into_iter().find_map(|event| match event.event {
        WorkspaceEvent::GitProgress(progress) if progress.completed => Some(progress.message),
        _ => None,
    }).expect("completed Git progress event");
    assert_eq!(completed_message, error.message);
}
