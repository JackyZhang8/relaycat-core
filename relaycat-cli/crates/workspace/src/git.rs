use crate::{ProjectRoot, WorkspaceServiceError};
use relaycat_protocol::{
    ContentPage, GitChange, GitCommitAction, GitCommitDetail, GitDiffTarget, GitHistoryItem,
    GitHistoryPage, GitMutationResult, GitRef, GitRemoteOperation, GitResetMode, GitStatus,
    GitSummary, WorkspaceErrorCode, WORKSPACE_GIT_HISTORY_PAGE_SIZE, WORKSPACE_PAYLOAD_BUDGET,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct GitService { root: ProjectRoot, mutation: Arc<Mutex<()>> }

impl GitService {
    pub fn new(root: ProjectRoot) -> Self { Self { root, mutation: Arc::new(Mutex::new(())) } }

    pub fn summary(&self) -> Result<GitSummary, WorkspaceServiceError> {
        self.ensure_repo()?;
        let branch = self.text(&["branch", "--show-current"])?.trim().to_string();
        let upstream_output = self.output(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"])?;
        let upstream = upstream_output.status.success().then(|| String::from_utf8_lossy(&upstream_output.stdout).trim().to_string()).filter(|v| !v.is_empty());
        let (ahead, behind) = if upstream.is_some() {
            let counts = self.text(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])?;
            let mut fields = counts.split_whitespace();
            (fields.next().unwrap_or("0").parse().unwrap_or(0), fields.next().unwrap_or("0").parse().unwrap_or(0))
        } else { (0, 0) };
        let status = self.status()?;
        let staged_count = status.changes.iter().filter(|change| change.staged).count() as u32;
        let unstaged_count = status.changes.iter().filter(|change| !change.staged && change.status != "untracked").count() as u32;
        let untracked_count = status.changes.iter().filter(|change| change.status == "untracked").count() as u32;
        Ok(GitSummary { branch, upstream, ahead, behind, staged_count, unstaged_count, untracked_count, fingerprint: status.fingerprint })
    }

    pub fn status(&self) -> Result<GitStatus, WorkspaceServiceError> {
        self.ensure_repo()?;
        let branch = self.text(&["branch", "--show-current"])?.trim().to_string();
        let output = self.output(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        self.success(&output)?;
        let mut changes = Vec::new();
        for raw in output.stdout.split(|byte| *byte == 0).filter(|part| !part.is_empty()) {
            if raw.len() < 3 { continue; }
            let x = raw[0] as char;
            let y = raw[1] as char;
            let path = String::from_utf8_lossy(&raw[3..]).into_owned();
            if x == '?' && y == '?' {
                changes.push(GitChange { path, old_path: None, status: "untracked".into(), staged: false });
            } else {
                if x != ' ' { changes.push(GitChange { path: path.clone(), old_path: None, status: status_name(x), staged: true }); }
                if y != ' ' { changes.push(GitChange { path, old_path: None, status: status_name(y), staged: false }); }
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| b.staged.cmp(&a.staged)));
        let mut hasher = Sha256::new();
        hasher.update(branch.as_bytes());
        for change in &changes {
            hasher.update(change.path.as_bytes()); hasher.update(change.status.as_bytes()); hasher.update([change.staged as u8]);
        }
        Ok(GitStatus { branch, changes, fingerprint: format!("{:x}", hasher.finalize()) })
    }

    pub fn diff(&self, target: GitDiffTarget, cursor: Option<String>, max_bytes: u32) -> Result<ContentPage, WorkspaceServiceError> {
        let mut args = vec!["diff".to_string(), "--no-ext-diff".into(), "--no-color".into()];
        match target {
            GitDiffTarget::WorkingTree => {}
            GitDiffTarget::Staged => args.push("--cached".into()),
            GitDiffTarget::Path { path, staged } => { self.validate_paths(std::slice::from_ref(&path))?; if staged { args.push("--cached".into()); } args.push("--".into()); args.push(path); }
            GitDiffTarget::Commit { oid } => { validate_oid(&oid)?; args.push(format!("{oid}^!")); }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.output(&refs)?; self.success(&output)?;
        Ok(content_page(output.stdout, cursor, max_bytes))
    }

    pub fn history(&self, query: &str, author: &str, reference: &str, cursor: Option<String>, limit: u16) -> Result<GitHistoryPage, WorkspaceServiceError> {
        self.ensure_repo()?;
        let offset = cursor.as_deref().unwrap_or("0").parse::<usize>().map_err(|_| WorkspaceServiceError::invalid("invalid history cursor"))?;
        let page = usize::from(limit.clamp(1, WORKSPACE_GIT_HISTORY_PAGE_SIZE));
        let mut args = vec!["log".to_string(), format!("--skip={offset}"), format!("-n{}", page + 1), "--date=unix".into(), "--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%at%x1f%D%x1f%P%x1e".into()];
        if !query.is_empty() { args.push(format!("--grep={query}")); }
        if !author.is_empty() { args.push(format!("--author={author}")); }
        if !reference.is_empty() { if reference.starts_with('-') { return Err(WorkspaceServiceError::invalid("invalid history reference")); } args.push(reference.into()); }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = self.text(&refs)?;
        let mut items: Vec<GitHistoryItem> = text.split('\x1e').filter_map(parse_history_item).collect();
        let has_more = items.len() > page; items.truncate(page);
        Ok(GitHistoryPage { items, next_cursor: has_more.then(|| (offset + page).to_string()) })
    }

    pub fn commit_detail(&self, oid: &str, cursor: Option<String>, max_bytes: u32) -> Result<GitCommitDetail, WorkspaceServiceError> {
        validate_oid(oid)?;
        let history = self.text(&["show", "-s", "--date=unix", "--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%at%x1f%D%x1f%P", oid])?;
        let item = parse_history_item(&history).ok_or_else(|| WorkspaceServiceError::invalid("invalid commit detail"))?;
        let body = self.text(&["show", "-s", "--pretty=format:%B", oid])?;
        let names = self.text(&["diff-tree", "--no-commit-id", "--name-status", "-r", oid])?;
        let changed_paths = names.lines().filter_map(|line| { let mut f=line.splitn(2,'\t'); Some(GitChange { status:f.next()?.to_lowercase(), path:f.next()?.to_string(), old_path:None, staged:false }) }).collect();
        let diff = self.diff(GitDiffTarget::Commit { oid: oid.into() }, cursor, max_bytes)?;
        Ok(GitCommitDetail { item, body, changed_paths, diff })
    }

    pub fn list_refs(&self) -> Result<Vec<GitRef>, WorkspaceServiceError> {
        let current = self.text(&["branch", "--show-current"])?.trim().to_string();
        let text = self.text(&["for-each-ref", "--format=%(refname)%09%(refname:short)%09%(objectname)", "refs/heads", "refs/remotes", "refs/tags"])?;
        Ok(text.lines().filter_map(|line| { let mut f=line.split('\t'); let name=f.next()?.to_string(); let short_name=f.next()?.to_string(); let oid=f.next()?.to_string(); let kind=if name.starts_with("refs/heads/"){"local"}else if name.starts_with("refs/tags/"){"tag"}else{"remote"}; Some(GitRef { current:kind=="local"&&short_name==current, name, short_name, kind:kind.into(), oid }) }).collect())
    }

    pub fn stage(&self, paths: &[String]) -> Result<GitMutationResult, WorkspaceServiceError> { self.paths_mutation(&["add"], paths) }
    pub fn unstage(&self, paths: &[String]) -> Result<GitMutationResult, WorkspaceServiceError> {
        let has_head = self.output(&["rev-parse", "--verify", "HEAD"])?.status.success();
        self.paths_mutation(if has_head { &["restore", "--staged"] } else { &["rm", "--cached", "--quiet", "--ignore-unmatch"] }, paths)
    }
    pub fn discard(&self, paths: &[String]) -> Result<GitMutationResult, WorkspaceServiceError> {
        let _guard = self.lock_mutation()?; self.validate_paths(paths)?;
        for path in paths {
            if self.output(&["ls-files", "--error-unmatch", "--", path])?.status.success() {
                self.run_success(&["restore", "--worktree", "--", path])?;
            } else { let target=self.root.resolve(path)?; if target.is_dir(){fs::remove_dir_all(target).map_err(WorkspaceServiceError::io)?}else{fs::remove_file(target).map_err(WorkspaceServiceError::io)?} }
        }
        self.mutation_result()
    }
    pub fn apply_patch(&self, patch: &str, cached: bool, reverse: bool) -> Result<GitMutationResult, WorkspaceServiceError> {
        if patch.is_empty() || patch.len() > 2*1024*1024 { return Err(WorkspaceServiceError::invalid("invalid patch size")); }
        let _guard=self.lock_mutation()?; let mut command=self.command(); command.args(["apply","--recount","--whitespace=nowarn"]); if cached{command.arg("--cached");} if reverse{command.arg("--reverse");}
        let mut child=command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(WorkspaceServiceError::io)?;
        child.stdin.take().ok_or_else(||WorkspaceServiceError::invalid("git apply stdin unavailable"))?.write_all(patch.as_bytes()).map_err(WorkspaceServiceError::io)?;
        let output=child.wait_with_output().map_err(WorkspaceServiceError::io)?; self.success(&output)?; self.mutation_result()
    }
    pub fn checkout(&self, reference:&str)->Result<GitMutationResult,WorkspaceServiceError>{if reference.is_empty()||reference.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid ref"));}self.simple_mutation(&["switch",reference])}
    pub fn create_branch(&self,name:&str,start:&str)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;if start.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid start point"));}let mut a=vec!["switch","-c",name];if !start.is_empty(){a.push(start);}self.simple_mutation(&a)}
    pub fn rename_branch(&self,name:&str)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;self.simple_mutation(&["branch","-m",name])}
    pub fn delete_branch(&self,name:&str,force:bool)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;self.simple_mutation(&["branch",if force{"-D"}else{"-d"},name])}
    pub fn create_tag(&self,name:&str,target:&str)->Result<GitMutationResult,WorkspaceServiceError>{if name.is_empty()||target.is_empty()||name.starts_with('-')||target.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid tag"));}self.simple_mutation(&["tag",name,target])}
    pub fn commit(&self,message:&str,amend:bool)->Result<GitMutationResult,WorkspaceServiceError>{if message.trim().is_empty()||message.len()>1024*1024{return Err(WorkspaceServiceError::invalid("invalid commit message"));}let nonce=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();let path=std::env::temp_dir().join(format!("relaycat-workspace-commit-{nonce}.txt"));fs::write(&path,message).map_err(WorkspaceServiceError::io)?;let p=path.to_string_lossy().into_owned();let result=if amend{self.simple_mutation(&["commit","-F",&p,"--amend"])}else{self.simple_mutation(&["commit","-F",&p])};let _=fs::remove_file(path);result}
    pub fn remote(&self,kind:GitRemoteOperation)->Result<GitMutationResult,WorkspaceServiceError>{let args:&[&str]=match kind{GitRemoteOperation::Fetch=>&["fetch","--progress","--prune"],GitRemoteOperation::Pull=>&["pull","--progress","--ff-only"],GitRemoteOperation::Push=>&["push","--progress"]};self.simple_mutation(args)}
    pub fn commit_action(&self,kind:GitCommitAction,oid:&str)->Result<GitMutationResult,WorkspaceServiceError>{validate_oid(oid)?;self.simple_mutation(&[match kind{GitCommitAction::Revert=>"revert",GitCommitAction::CherryPick=>"cherry-pick"},oid])}
    pub fn reset(&self,oid:&str,mode:GitResetMode)->Result<GitMutationResult,WorkspaceServiceError>{validate_oid(oid)?;self.simple_mutation(&["reset",match mode{GitResetMode::Soft=>"--soft",GitResetMode::Mixed=>"--mixed",GitResetMode::Hard=>"--hard"},oid])}

    fn paths_mutation(&self,prefix:&[&str],paths:&[String])->Result<GitMutationResult,WorkspaceServiceError>{let _guard=self.lock_mutation()?;self.validate_paths(paths)?;let mut args:Vec<&str>=prefix.to_vec();args.push("--");args.extend(paths.iter().map(String::as_str));self.run_success(&args)?;self.mutation_result()}
    fn simple_mutation(&self,args:&[&str])->Result<GitMutationResult,WorkspaceServiceError>{let _guard=self.lock_mutation()?;self.run_success(args)?;self.mutation_result()}
    fn mutation_result(&self)->Result<GitMutationResult,WorkspaceServiceError>{Ok(GitMutationResult{operation_id:format!("git-{}",SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()),summary:self.summary()?})}
    fn lock_mutation(&self)->Result<std::sync::MutexGuard<'_,()>,WorkspaceServiceError>{self.mutation.try_lock().map_err(|_|WorkspaceServiceError::busy())}
    fn validate_paths(&self,paths:&[String])->Result<(),WorkspaceServiceError>{if paths.is_empty(){return Err(WorkspaceServiceError::invalid("at least one path is required"));}for path in paths{self.root.lexical_path(path)?;}Ok(())}
    fn validate_branch(&self,name:&str)->Result<(),WorkspaceServiceError>{if name.is_empty(){return Err(WorkspaceServiceError::invalid("branch name is required"));}let output=self.output(&["check-ref-format","--branch",name])?;if output.status.success(){Ok(())}else{Err(WorkspaceServiceError::invalid("invalid branch name"))}}
    fn ensure_repo(&self)->Result<(),WorkspaceServiceError>{let output=self.output(&["rev-parse","--git-dir"])?;if output.status.success(){Ok(())}else{Err(WorkspaceServiceError::new(WorkspaceErrorCode::NotGitRepository,"current project is not a Git repository",false))}}
    fn command(&self)->Command{let mut command=Command::new("git");command.current_dir(self.root.path()).env("GIT_TERMINAL_PROMPT","0").env("GCM_INTERACTIVE","Never").env("LC_ALL","C");command}
    fn output(&self,args:&[&str])->Result<Output,WorkspaceServiceError>{self.command().args(args).output().map_err(WorkspaceServiceError::io)}
    fn text(&self,args:&[&str])->Result<String,WorkspaceServiceError>{let output=self.output(args)?;self.success(&output)?;Ok(String::from_utf8_lossy(&output.stdout).into_owned())}
    fn run_success(&self,args:&[&str])->Result<(),WorkspaceServiceError>{let output=self.output(args)?;self.success(&output)}
    fn success(&self,output:&Output)->Result<(),WorkspaceServiceError>{if output.status.success(){Ok(())}else{let message=String::from_utf8_lossy(&output.stderr).trim().to_string();Err(WorkspaceServiceError::new(WorkspaceErrorCode::Conflict,if message.is_empty(){"Git command failed".into()}else{message},false))}}
}

fn status_name(code:char)->String{match code{'A'=>"added",'D'=>"deleted",'R'=>"renamed",'C'=>"copied",'U'=>"conflict",_=>"modified"}.into()}
fn validate_oid(oid:&str)->Result<(),WorkspaceServiceError>{if(4..=64).contains(&oid.len())&&oid.bytes().all(|b|b.is_ascii_hexdigit()){Ok(())}else{Err(WorkspaceServiceError::invalid("invalid commit id"))}}
fn parse_history_item(raw:&str)->Option<GitHistoryItem>{let fields:Vec<&str>=raw.trim_matches(['\n','\r']).split('\x1f').collect();if fields.len()<7{return None;}Some(GitHistoryItem{oid:fields[0].into(),short_oid:fields[1].into(),subject:fields[2].into(),author:fields[3].into(),authored_at_unix_ms:fields[4].parse::<u64>().ok()?.saturating_mul(1000),decorations:fields[5].split(',').map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).collect(),parent_oids:fields[6].split_whitespace().map(str::to_string).collect()})}
fn content_page(bytes:Vec<u8>,cursor:Option<String>,max_bytes:u32)->ContentPage{let start=cursor.as_deref().unwrap_or("0").parse::<usize>().unwrap_or(0).min(bytes.len());let limit=(max_bytes as usize).clamp(1,WORKSPACE_PAYLOAD_BUDGET);let mut end=start.saturating_add(limit).min(bytes.len());while end>start&&std::str::from_utf8(&bytes[start..end]).is_err(){end-=1;}let truncated=end<bytes.len();ContentPage{bytes:bytes[start..end].to_vec(),next_cursor:truncated.then(||end.to_string()),truncated}}
