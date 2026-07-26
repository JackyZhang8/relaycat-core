use crate::{FileService, GitService, ProjectRoot, ShellManager, ShellTransportEvent, WorkspaceServiceError, IMAGE_PREVIEW_LIMIT, TEXT_PREVIEW_LIMIT};
use relaycat_protocol::{
    GitProgressEvent, ShellDescriptor, ShellSnapshot, WorkspaceCapabilities, WorkspaceError, WorkspaceEvent, WorkspaceEventEnvelope,
    WorkspaceRequest, WorkspaceRequestEnvelope, WorkspaceResponse, WorkspaceResponseEnvelope,
    WORKSPACE_APP_SHELL_LIMIT, WORKSPACE_DIRECTORY_ENTRY_LIMIT, WORKSPACE_DIRECTORY_PAGE_SIZE,
    WORKSPACE_GIT_HISTORY_PAGE_SIZE,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const IDEMPOTENCY_TTL: Duration = Duration::from_secs(10 * 60);
const IDEMPOTENCY_LIMIT: usize = 256;

#[derive(Clone)]
pub struct WorkspaceService {
    root: ProjectRoot,
    files: FileService,
    git: GitService,
    shells: ShellManager,
    cache: Arc<Mutex<HashMap<String, (Instant, std::result::Result<WorkspaceResponse, WorkspaceError>)>>>,
    cache_order: Arc<Mutex<VecDeque<String>>>,
    events: Arc<Mutex<VecDeque<WorkspaceEventEnvelope>>>,
    event_seq: Arc<Mutex<u64>>,
}

impl WorkspaceService {
    pub fn new(root: ProjectRoot, shell_limit: u8) -> Self {
        Self {
            files: FileService::new(root.clone()), git: GitService::new(root.clone()),
            shells: ShellManager::new(root.clone(), shell_limit.min(WORKSPACE_APP_SHELL_LIMIT)), root,
            cache: Arc::new(Mutex::new(HashMap::new())), cache_order: Arc::new(Mutex::new(VecDeque::new())),
            events: Arc::new(Mutex::new(VecDeque::new())), event_seq: Arc::new(Mutex::new(0)),
        }
    }

    pub fn project_id(&self) -> &str { self.root.project_id() }

    pub fn shell_descriptors(&self) -> Vec<ShellDescriptor> { self.shells.list() }

    pub fn ensure_workspace_shell(&self, cols: u16, rows: u16) -> Result<ShellDescriptor, WorkspaceServiceError> {
        self.ensure_workspace_shell_snapshot(cols, rows).map(|snapshot| snapshot.descriptor)
    }

    fn ensure_workspace_shell_snapshot(&self, cols: u16, rows: u16) -> Result<ShellSnapshot, WorkspaceServiceError> {
        if let Some(descriptor) = self.shells.list().into_iter().next() {
            return Ok(ShellSnapshot {
                first_output_seq: descriptor.last_output_seq,
                last_output_seq: descriptor.last_output_seq,
                descriptor,
                bytes: Vec::new(),
                complete_screen: false,
            });
        }
        self.shells.create(cols, rows)
    }

    pub fn drain_shell_transport_events(&self) -> Vec<ShellTransportEvent> {
        self.shells.drain_transport_events()
    }

    pub fn write_workspace_shell(&self, bytes: Vec<u8>) -> Result<(), WorkspaceServiceError> {
        self.shells.write_active(bytes)
    }

    pub fn resize_workspace_shell(&self, cols: u16, rows: u16) -> Result<(), WorkspaceServiceError> {
        self.shells.resize_active(cols, rows)
    }

    pub fn close_workspace_shell(&self) -> Result<(), WorkspaceServiceError> {
        let Some(shell_id) = self.shells.active_shell_id() else { return Ok(()); };
        self.shells.close(&shell_id)
    }

    pub fn execute(&self, request: WorkspaceRequestEnvelope) -> WorkspaceResponseEnvelope {
        let request_id=request.request_id.clone(); let project_id=self.root.project_id().to_string();
        if request.project_id != project_id {
            return WorkspaceResponseEnvelope{request_id,project_id,result:Err(WorkspaceServiceError::outside_project().into())};
        }
        if request.deadline_unix_ms < unix_ms() {
            return WorkspaceResponseEnvelope{request_id,project_id,result:Err(WorkspaceServiceError::timeout().into())};
        }
        if let Some(key)=request.idempotency_key.as_ref() {
            if let Some(result)=self.cached(key) { return WorkspaceResponseEnvelope{request_id,project_id,result}; }
        }
        let result=self.dispatch(request.operation);
        if let Some(key)=request.idempotency_key { self.remember(key,result.clone()); }
        WorkspaceResponseEnvelope{request_id,project_id,result}
    }

    pub fn drain_events(&self) -> Vec<WorkspaceEventEnvelope> {
        self.events.lock().map(|mut events|events.drain(..).collect()).unwrap_or_default()
    }

    pub fn shutdown(&self) { let _=self.shells.close_all(); }

    fn dispatch(&self, operation:WorkspaceRequest)->std::result::Result<WorkspaceResponse,WorkspaceError>{
        let result:Result<WorkspaceResponse,WorkspaceServiceError>=match operation {
            WorkspaceRequest::Capabilities=>Ok(WorkspaceResponse::Capabilities(WorkspaceCapabilities{files:true,git_read:true,git_write:true,shell:true,directory_page_size:WORKSPACE_DIRECTORY_PAGE_SIZE,directory_entry_limit:WORKSPACE_DIRECTORY_ENTRY_LIMIT,git_history_page_size:WORKSPACE_GIT_HISTORY_PAGE_SIZE,text_preview_limit:TEXT_PREVIEW_LIMIT as u32,image_preview_limit:IMAGE_PREVIEW_LIMIT as u32,shell_limit:WORKSPACE_APP_SHELL_LIMIT})),
            WorkspaceRequest::Cancel{..}=>Ok(WorkspaceResponse::Ack),
            WorkspaceRequest::ListDirectory{path,offset,limit}=>self.files.list(&path,offset,limit).map(WorkspaceResponse::Directory),
            WorkspaceRequest::ReadFile{path,max_bytes,image_variant}=>self.files.read(&path,max_bytes,image_variant).map(WorkspaceResponse::File),
            WorkspaceRequest::GitSummary=>self.git.summary().map(WorkspaceResponse::GitSummary),
            WorkspaceRequest::GitStatus=>self.git.status().map(WorkspaceResponse::GitStatus),
            WorkspaceRequest::GitDiff{target,cursor,max_bytes}=>self.git.diff(target,cursor,max_bytes).map(WorkspaceResponse::GitDiff),
            WorkspaceRequest::GitHistory{query,author,reference,cursor,limit}=>self.git.history(&query,&author,&reference,cursor,limit).map(WorkspaceResponse::GitHistory),
            WorkspaceRequest::GitCommitDetail{oid,cursor,max_bytes}=>self.git.commit_detail(&oid,cursor,max_bytes).map(WorkspaceResponse::GitCommitDetail),
            WorkspaceRequest::GitListRefs=>self.git.list_refs().map(WorkspaceResponse::GitRefs),
            WorkspaceRequest::GitStage{paths}=>self.git.stage(&paths).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitUnstage{paths}=>self.git.unstage(&paths).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitDiscard{paths}=>self.git.discard(&paths).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitApplyPatch{patch,reverse,cached}=>self.git.apply_patch(&patch,cached,reverse).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitCheckout{reference}=>self.git.checkout(&reference).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitCreateBranch{name,start_point}=>self.git.create_branch(&name,&start_point).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitRenameBranch{name}=>self.git.rename_branch(&name).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitDeleteBranch{name,force}=>self.git.delete_branch(&name,force).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitCreateTag{name,target}=>self.git.create_tag(&name,&target).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitCommit{message,amend,push_after}=>self.git.commit(&message,amend).and_then(|commit|if push_after{self.git.remote(relaycat_protocol::GitRemoteOperation::Push)}else{Ok(commit)}).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitRemote{kind}=>self.git_remote(kind),
            WorkspaceRequest::GitCommitAction{kind,oid}=>self.git.commit_action(kind,&oid).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::GitReset{oid,mode}=>self.git.reset(&oid,mode).map(WorkspaceResponse::GitMutation),
            WorkspaceRequest::ShellList=>Ok(WorkspaceResponse::ShellList(self.shells.list())),
            WorkspaceRequest::ShellCreate{cols,rows}=>self.ensure_workspace_shell_snapshot(cols,rows).map(WorkspaceResponse::ShellCreated),
            WorkspaceRequest::ShellInput{..}|WorkspaceRequest::ShellResize{..}|WorkspaceRequest::ShellSnapshot{..}=>Err(WorkspaceServiceError::unsupported("shell synchronization requires terminal_stream_v2")),
            WorkspaceRequest::ShellClose{shell_id}=>self.shells.close(&shell_id).map(|_|WorkspaceResponse::Ack),
            WorkspaceRequest::ShellCloseAll=>self.shells.close_all().map(|_|WorkspaceResponse::Ack),
        }; result.map_err(Into::into)
    }

    fn git_remote(&self,kind:relaycat_protocol::GitRemoteOperation)->Result<WorkspaceResponse,WorkspaceServiceError>{
        let operation_id=format!("{:?}-{}",kind,unix_ms());
        match self.git.remote_with_summary(kind){
            Ok(outcome)=>{
                let last=outcome.messages.len().saturating_sub(1);
                for(index,message)in outcome.messages.into_iter().enumerate(){let completed=index==last;self.push_event(WorkspaceEvent::GitProgress(GitProgressEvent{operation_id:operation_id.clone(),phase:if completed{"completed"}else{"summary"}.into(),message,percent:completed.then_some(100),completed,success:completed.then_some(true)}));}
                Ok(WorkspaceResponse::GitMutation(outcome.mutation))
            }
            Err(error)=>{
                self.push_event(WorkspaceEvent::GitProgress(GitProgressEvent{operation_id,phase:"completed".into(),message:format!("[{:?}] FAILED · {}",kind,error),percent:Some(100),completed:true,success:Some(false)}));
                Err(error)
            }
        }
    }

    fn cached(&self,key:&str)->Option<std::result::Result<WorkspaceResponse,WorkspaceError>>{let now=Instant::now();let mut cache=self.cache.lock().ok()?;cache.retain(|_,(created,_)|now.duration_since(*created)<IDEMPOTENCY_TTL);cache.get(key).map(|(_,result)|result.clone())}
    fn remember(&self,key:String,result:std::result::Result<WorkspaceResponse,WorkspaceError>){if let(Ok(mut cache),Ok(mut order))=(self.cache.lock(),self.cache_order.lock()){if !cache.contains_key(&key){order.push_back(key.clone());}cache.insert(key,(Instant::now(),result));while order.len()>IDEMPOTENCY_LIMIT{if let Some(old)=order.pop_front(){cache.remove(&old);}}}}
    fn push_event(&self,event:WorkspaceEvent){if let Ok(mut seq)=self.event_seq.lock(){*seq=seq.saturating_add(1);if let Ok(mut events)=self.events.lock(){events.push_back(WorkspaceEventEnvelope{project_id:self.root.project_id().into(),event_seq:*seq,event});}}}
}

fn unix_ms()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u128::from(u64::MAX)) as u64}
