use relaycat_protocol::{PlainMsg, WorkspaceRequest, WorkspaceRequestEnvelope, WorkspaceResponse};
use relaycat_workspace::{ProjectRoot, WorkspaceService};
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
