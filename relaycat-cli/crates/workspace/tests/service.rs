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
fn git_output(root:&PathBuf,args:&[&str])->String{let output=Command::new("git").args(args).current_dir(root).output().unwrap();assert!(output.status.success(),"git {args:?} failed: {}",String::from_utf8_lossy(&output.stderr));String::from_utf8_lossy(&output.stdout).trim().to_string()}
fn configure_user(root:&PathBuf){git(root,&["config","user.name","RelayCat Test"]);git(root,&["config","user.email","relaycat@example.test"]);}
fn progress_messages(service:&WorkspaceService)->Vec<String>{service.drain_events().into_iter().filter_map(|event|match event.event{WorkspaceEvent::GitProgress(progress)=>Some(progress.message),_=>None}).collect()}

struct RemoteFixture { remote: PathBuf, peer: PathBuf }
impl RemoteFixture {
    fn attach(repo:&Repo)->Self{
        let parent=repo.root.parent().unwrap();
        let suffix=NEXT_REPO_ID.fetch_add(1,Ordering::Relaxed);
        let remote=parent.join(format!("relaycat-remote-{}-{suffix}.git",std::process::id()));
        let peer=parent.join(format!("relaycat-peer-{}-{suffix}",std::process::id()));
        fs::create_dir_all(&remote).unwrap();
        git(&remote,&["init","--bare","--quiet"]);
        git(&repo.root,&["remote","add","origin",remote.to_str().unwrap()]);
        git(&repo.root,&["push","--quiet","-u","origin","HEAD"]);
        assert!(Command::new("git").args(["clone","--quiet",remote.to_str().unwrap(),peer.to_str().unwrap()]).status().unwrap().success());
        configure_user(&peer);
        Self{remote,peer}
    }

    fn peer_commit(&self,file:&str,contents:&str,message:&str){fs::write(self.peer.join(file),contents).unwrap();git(&self.peer,&["add","--",file]);git(&self.peer,&["commit","--quiet","-m",message]);git(&self.peer,&["push","--quiet"]);}
}
impl Drop for RemoteFixture{fn drop(&mut self){let _=fs::remove_dir_all(&self.peer);let _=fs::remove_dir_all(&self.remote);}}

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
    assert!(completed_message.starts_with("[Fetch] FAILED · "));
    assert!(completed_message.contains(&error.message));
}

#[test]
fn git_remote_progress_reports_bounded_fetch_pull_and_push_summaries() {
    let repo=Repo::new();
    let remote=RemoteFixture::attach(&repo);
    let root=ProjectRoot::open(&repo.root).unwrap();
    let project_id=root.project_id().to_string();
    let service=WorkspaceService::new(root,1);

    remote.peer_commit("remote.txt","first\nsecond\n","remote change");
    let fetch=service.execute(WorkspaceRequestEnvelope{request_id:"fetch".into(),project_id:project_id.clone(),deadline_unix_ms:u64::MAX,idempotency_key:None,operation:WorkspaceRequest::GitRemote{kind:GitRemoteOperation::Fetch}});
    assert!(fetch.result.is_ok());
    let fetch_messages=progress_messages(&service);
    assert!((1..=2).contains(&fetch_messages.len()),"{fetch_messages:?}");
    assert!(fetch_messages.iter().any(|message|message.contains("[Fetch] OK")&&message.contains("origin/")&&message.contains('→')),"{fetch_messages:?}");

    let pull=service.execute(WorkspaceRequestEnvelope{request_id:"pull".into(),project_id:project_id.clone(),deadline_unix_ms:u64::MAX,idempotency_key:None,operation:WorkspaceRequest::GitRemote{kind:GitRemoteOperation::Pull}});
    assert!(pull.result.is_ok());
    let pull_messages=progress_messages(&service);
    assert!((1..=3).contains(&pull_messages.len()),"{pull_messages:?}");
    assert!(pull_messages.iter().any(|message|message.contains("[Pull] OK")&&message.contains("1 commit")&&message.contains("1 file")&&message.contains("+2/-0")),"{pull_messages:?}");
    assert!(pull_messages.iter().any(|message|message.contains("remote change")),"{pull_messages:?}");

    fs::write(repo.root.join("local.txt"),"local\n").unwrap();
    git(&repo.root,&["add","--","local.txt"]);
    git(&repo.root,&["commit","--quiet","-m","local change"]);
    let head=git_output(&repo.root,&["rev-parse","--short=7","HEAD"]);
    let push=service.execute(WorkspaceRequestEnvelope{request_id:"push".into(),project_id,deadline_unix_ms:u64::MAX,idempotency_key:None,operation:WorkspaceRequest::GitRemote{kind:GitRemoteOperation::Push}});
    assert!(push.result.is_ok());
    let push_messages=progress_messages(&service);
    assert_eq!(push_messages.len(),1,"{push_messages:?}");
    assert!(push_messages[0].contains("[Push]")&&push_messages[0].contains("1 commit")&&push_messages[0].contains(&head)&&push_messages[0].ends_with("OK"),"{push_messages:?}");
}
