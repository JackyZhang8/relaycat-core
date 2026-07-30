use crate::{ProjectRoot, WorkspaceServiceError};
use relaycat_protocol::{
    ContentPage, GitChange, GitCommitAction, GitCommitDetail, GitDiffTarget, GitHistoryItem,
    GitHistoryPage, GitMutationResult, GitRef, GitRemoteOperation, GitResetMode, GitStatus,
    GitSummary, WorkspaceErrorCode, WORKSPACE_GIT_HISTORY_PAGE_SIZE, WORKSPACE_PAYLOAD_BUDGET,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const GIT_ERROR_MESSAGE_LIMIT: usize = 4 * 1024;

#[derive(Clone)]
pub struct GitService { root: ProjectRoot, mutation: Arc<Mutex<()>> }

pub(crate) struct GitRemoteOutcome {
    pub mutation: GitMutationResult,
    pub messages: Vec<String>,
}

#[derive(Default)]
struct GitRemoteSnapshot {
    branch: String,
    upstream: Option<String>,
    remote: String,
    head: Option<String>,
    upstream_head: Option<String>,
    ahead: u32,
    remote_refs: BTreeMap<String, String>,
}

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
        let output = self.output(&["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "."])?;
        self.success(&output)?;
        let repository_root = self.repository_root()?;
        let mut changes = Vec::new();
        let records: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).filter(|part| !part.is_empty()).collect();
        let mut index = 0;
        while index < records.len() {
            let raw = records[index];
            if raw.len() < 3 { index += 1; continue; }
            let x = raw[0] as char;
            let y = raw[1] as char;
            let path = self.project_relative_git_path(&repository_root, &String::from_utf8_lossy(&raw[3..]))?;
            let renamed_or_copied = matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C');
            let old_path = if renamed_or_copied {
                index += 1;
                let raw_old_path = records.get(index).ok_or_else(|| WorkspaceServiceError::invalid("missing original path for renamed git entry"))?;
                Some(self.project_relative_git_path(&repository_root, &String::from_utf8_lossy(raw_old_path))?)
            } else {
                None
            };
            if x == '?' && y == '?' {
                changes.push(GitChange { path, old_path: None, status: "untracked".into(), staged: false });
            } else {
                if x != ' ' { changes.push(GitChange { path: path.clone(), old_path: old_path.clone(), status: status_name(x), staged: true }); }
                if y != ' ' { changes.push(GitChange { path, old_path, status: status_name(y), staged: false }); }
            }
            index += 1;
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| b.staged.cmp(&a.staged)));
        let mut hasher = Sha256::new();
        hasher.update(branch.as_bytes());
        for change in &changes {
            hasher.update(change.path.as_bytes()); hasher.update(change.status.as_bytes()); hasher.update([change.staged as u8]);
            if let Some(old_path) = &change.old_path { hasher.update(old_path.as_bytes()); }
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
        let mut child=command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(map_git_command_error)?;
        child.stdin.take().ok_or_else(||WorkspaceServiceError::invalid("git apply stdin unavailable"))?.write_all(patch.as_bytes()).map_err(WorkspaceServiceError::io)?;
        let output=child.wait_with_output().map_err(WorkspaceServiceError::io)?; self.success(&output)?; self.mutation_result()
    }
    pub fn checkout(&self, reference:&str)->Result<GitMutationResult,WorkspaceServiceError>{if reference.is_empty()||reference.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid ref"));}self.simple_mutation(&["switch",reference])}
    pub fn create_branch(&self,name:&str,start:&str)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;if start.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid start point"));}let mut a=vec!["switch","-c",name];if !start.is_empty(){a.push(start);}self.simple_mutation(&a)}
    pub fn rename_branch(&self,name:&str)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;self.simple_mutation(&["branch","-m",name])}
    pub fn delete_branch(&self,name:&str,force:bool)->Result<GitMutationResult,WorkspaceServiceError>{self.validate_branch(name)?;self.simple_mutation(&["branch",if force{"-D"}else{"-d"},name])}
    pub fn create_tag(&self,name:&str,target:&str)->Result<GitMutationResult,WorkspaceServiceError>{if name.is_empty()||target.is_empty()||name.starts_with('-')||target.starts_with('-'){return Err(WorkspaceServiceError::invalid("invalid tag"));}self.simple_mutation(&["tag",name,target])}
    pub fn commit(&self,message:&str,amend:bool)->Result<GitMutationResult,WorkspaceServiceError>{if message.trim().is_empty()||message.len()>1024*1024{return Err(WorkspaceServiceError::invalid("invalid commit message"));}let nonce=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();let path=std::env::temp_dir().join(format!("relaycat-workspace-commit-{nonce}.txt"));fs::write(&path,message).map_err(WorkspaceServiceError::io)?;let p=path.to_string_lossy().into_owned();let result=if amend{self.simple_mutation(&["commit","-F",&p,"--amend"])}else{self.simple_mutation(&["commit","-F",&p])};let _=fs::remove_file(path);result}
    pub fn remote(&self,kind:GitRemoteOperation)->Result<GitMutationResult,WorkspaceServiceError>{self.remote_with_summary(kind).map(|outcome|outcome.mutation)}
    pub(crate) fn remote_with_summary(&self,kind:GitRemoteOperation)->Result<GitRemoteOutcome,WorkspaceServiceError>{
        let args:&[&str]=match kind{GitRemoteOperation::Fetch=>&["fetch","--progress","--prune"],GitRemoteOperation::Pull=>&["pull","--progress","--ff-only"],GitRemoteOperation::Push=>&["push","--progress"]};
        let _guard=self.lock_mutation()?;
        let before=self.remote_snapshot();
        self.run_success(args)?;
        let after=self.remote_snapshot();
        let messages=self.remote_messages(kind,&before,&after);
        Ok(GitRemoteOutcome{mutation:self.mutation_result()?,messages})
    }
    pub fn commit_action(&self,kind:GitCommitAction,oid:&str)->Result<GitMutationResult,WorkspaceServiceError>{validate_oid(oid)?;self.simple_mutation(&[match kind{GitCommitAction::Revert=>"revert",GitCommitAction::CherryPick=>"cherry-pick"},oid])}
    pub fn reset(&self,oid:&str,mode:GitResetMode)->Result<GitMutationResult,WorkspaceServiceError>{validate_oid(oid)?;self.simple_mutation(&["reset",match mode{GitResetMode::Soft=>"--soft",GitResetMode::Mixed=>"--mixed",GitResetMode::Hard=>"--hard"},oid])}

    fn paths_mutation(&self,prefix:&[&str],paths:&[String])->Result<GitMutationResult,WorkspaceServiceError>{let _guard=self.lock_mutation()?;self.validate_paths(paths)?;let mut args:Vec<&str>=prefix.to_vec();args.push("--");args.extend(paths.iter().map(String::as_str));self.run_success(&args)?;self.mutation_result()}
    fn simple_mutation(&self,args:&[&str])->Result<GitMutationResult,WorkspaceServiceError>{let _guard=self.lock_mutation()?;self.run_success(args)?;self.mutation_result()}
    fn remote_snapshot(&self)->GitRemoteSnapshot{
        let branch=self.text(&["branch","--show-current"]).unwrap_or_default().trim().to_string();
        let upstream=self.output(&["rev-parse","--abbrev-ref","--symbolic-full-name","@{upstream}"]).ok().filter(|output|output.status.success()).map(|output|String::from_utf8_lossy(&output.stdout).trim().to_string()).filter(|value|!value.is_empty());
        let remote=upstream.as_deref().and_then(|value|value.split('/').next()).filter(|value|!value.is_empty()).map(str::to_string).or_else(||self.text(&["remote"]).ok().and_then(|value|value.lines().next().map(str::to_string))).unwrap_or_else(||"origin".into());
        let head=self.optional_oid("HEAD");
        let upstream_head=upstream.as_deref().and_then(|reference|self.optional_oid(reference));
        let ahead=if upstream.is_some(){self.text(&["rev-list","--count","@{upstream}..HEAD"]).ok().and_then(|value|value.trim().parse().ok()).unwrap_or(0)}else{0};
        let remote_refs=self.text(&["for-each-ref","--format=%(refname:short)%09%(objectname)","refs/remotes"]).ok().map(|text|text.lines().filter_map(|line|{let(name,oid)=line.split_once('\t')?;Some((name.to_string(),oid.to_string()))}).collect()).unwrap_or_default();
        GitRemoteSnapshot{branch,upstream,remote,head,upstream_head,ahead,remote_refs}
    }
    fn optional_oid(&self,reference:&str)->Option<String>{self.output(&["rev-parse","--verify",reference]).ok().filter(|output|output.status.success()).map(|output|String::from_utf8_lossy(&output.stdout).trim().to_string()).filter(|value|!value.is_empty())}
    fn remote_messages(&self,kind:GitRemoteOperation,before:&GitRemoteSnapshot,after:&GitRemoteSnapshot)->Vec<String>{
        match kind{
            GitRemoteOperation::Fetch=>fetch_messages(before,after),
            GitRemoteOperation::Pull=>pull_messages(self,before,after),
            GitRemoteOperation::Push=>push_messages(before),
        }
    }
    fn mutation_result(&self)->Result<GitMutationResult,WorkspaceServiceError>{Ok(GitMutationResult{operation_id:format!("git-{}",SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()),summary:self.summary()?})}
    fn lock_mutation(&self)->Result<std::sync::MutexGuard<'_,()>,WorkspaceServiceError>{self.mutation.try_lock().map_err(|_|WorkspaceServiceError::busy())}
    fn repository_root(&self)->Result<PathBuf,WorkspaceServiceError>{
        let root=self.text(&["rev-parse","--show-toplevel"])?;
        PathBuf::from(root.trim()).canonicalize().map_err(WorkspaceServiceError::io)
    }
    fn project_relative_git_path(&self,repository_root:&Path,path:&str)->Result<String,WorkspaceServiceError>{
        let absolute=repository_root.join(path);
        let relative=absolute.strip_prefix(self.root.path()).map_err(|_|WorkspaceServiceError::outside_project())?;
        Ok(relative.to_string_lossy().replace('\\',"/"))
    }
    fn validate_paths(&self,paths:&[String])->Result<(),WorkspaceServiceError>{if paths.is_empty(){return Err(WorkspaceServiceError::invalid("at least one path is required"));}for path in paths{self.root.lexical_path(path)?;}Ok(())}
    fn validate_branch(&self,name:&str)->Result<(),WorkspaceServiceError>{if name.is_empty(){return Err(WorkspaceServiceError::invalid("branch name is required"));}let output=self.output(&["check-ref-format","--branch",name])?;if output.status.success(){Ok(())}else{Err(WorkspaceServiceError::invalid("invalid branch name"))}}
    fn ensure_repo(&self)->Result<(),WorkspaceServiceError>{let output=self.output(&["rev-parse","--git-dir"])?;if output.status.success(){Ok(())}else{Err(WorkspaceServiceError::new(WorkspaceErrorCode::NotGitRepository,"current project is not a Git repository",false))}}
    fn command(&self)->Command{let mut command=Command::new("git");command.current_dir(self.root.path()).env("GIT_TERMINAL_PROMPT","0").env("GCM_INTERACTIVE","Never").env("LC_ALL","C");command}
    fn output(&self,args:&[&str])->Result<Output,WorkspaceServiceError>{self.command().args(args).output().map_err(map_git_command_error)}
    fn text(&self,args:&[&str])->Result<String,WorkspaceServiceError>{let output=self.output(args)?;self.success(&output)?;Ok(String::from_utf8_lossy(&output.stdout).into_owned())}
    fn run_success(&self,args:&[&str])->Result<(),WorkspaceServiceError>{let output=self.output(args)?;self.success(&output)}
    fn success(&self,output:&Output)->Result<(),WorkspaceServiceError>{if output.status.success(){Ok(())}else{let message=sanitize_git_error_message(&String::from_utf8_lossy(&output.stderr));Err(WorkspaceServiceError::new(WorkspaceErrorCode::Conflict,message,false))}}
}

fn status_name(code:char)->String{match code{'A'=>"added",'D'=>"deleted",'R'=>"renamed",'C'=>"copied",'U'=>"conflict",_=>"modified"}.into()}
fn map_git_command_error(error: std::io::Error) -> WorkspaceServiceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        WorkspaceServiceError::new(
            WorkspaceErrorCode::GitNotInstalled,
            "git is not installed",
            false,
        )
    } else {
        WorkspaceServiceError::io(error)
    }
}
fn short_oid(oid:&str)->&str{oid.get(..7).unwrap_or(oid)}
fn plural(count:u32,singular:&str,plural:&str)->String{format!("{count} {}",if count==1{singular}else{plural})}
fn remote_target(snapshot:&GitRemoteSnapshot)->String{snapshot.upstream.clone().unwrap_or_else(||if snapshot.branch.is_empty(){snapshot.remote.clone()}else{format!("{}/{}",snapshot.remote,snapshot.branch)})}
fn fetch_messages(before:&GitRemoteSnapshot,after:&GitRemoteSnapshot)->Vec<String>{
    let target=remote_target(after);
    let changes:Vec<String>=after.remote_refs.iter().filter_map(|(name,new_oid)|{
        let old_oid=before.remote_refs.get(name)?;
        (old_oid!=new_oid).then(||format!("{name} {} → {}",short_oid(old_oid),short_oid(new_oid)))
    }).take(3).collect();
    let created=after.remote_refs.keys().filter(|name|!before.remote_refs.contains_key(*name)).count();
    let changed_total=after.remote_refs.iter().filter(|(name,oid)|before.remote_refs.get(*name)!=Some(*oid)).count();
    let mut messages=vec![format!("[Fetch] {target}")];
    if changed_total==0{
        messages.push("[Fetch] OK · remote refs unchanged".into());
    }else{
        let mut detail=changes;
        if created>0&&detail.len()<3{detail.push(format!("{created} new remote {}",if created==1{"ref"}else{"refs"}));}
        let suffix=if detail.is_empty(){String::new()}else{format!(" · {}",detail.join("; "))};
        messages.push(format!("[Fetch] OK · {}{suffix}",plural(changed_total as u32,"remote ref","remote refs")));
    }
    messages
}
fn pull_messages(git:&GitService,before:&GitRemoteSnapshot,after:&GitRemoteSnapshot)->Vec<String>{
    let mut messages=vec![format!("[Pull] {}",remote_target(after))];
    let(Some(old_head),Some(new_head))=(before.head.as_deref(),after.head.as_deref())else{messages.push("[Pull] OK".into());return messages;};
    if old_head==new_head{messages.push("[Pull] OK · already up to date".into());return messages;}
    let range=format!("{old_head}..{new_head}");
    let commits=git.text(&["rev-list","--count",&range]).ok().and_then(|value|value.trim().parse::<u32>().ok()).unwrap_or(0);
    let (files,insertions,deletions)=git.text(&["diff","--shortstat",old_head,new_head]).ok().map(|value|parse_shortstat(&value)).unwrap_or_default();
    messages.push(format!("[Pull] OK · {} · {} · +{insertions}/-{deletions}",plural(commits,"commit","commits"),plural(files,"file","files")));
    if let Ok(log)=git.text(&["log","--format=%h%x09%s","-n","3",&range]){
        let subjects:Vec<String>=log.lines().filter_map(|line|line.split_once('\t').map(|(oid,subject)|format!("{oid} {subject}"))).collect();
        if !subjects.is_empty(){let remaining=commits.saturating_sub(subjects.len() as u32);let suffix=if remaining>0{format!("; {remaining} more")}else{String::new()};messages.push(format!("[Pull] {}{suffix}",subjects.join("; ")));}
    }
    messages.truncate(3);
    messages
}
fn push_messages(before:&GitRemoteSnapshot)->Vec<String>{
    let target=remote_target(before);
    if before.ahead==0{return vec![format!("[Push] {target} · OK · nothing to push")];}
    let range=match(before.upstream_head.as_deref(),before.head.as_deref()){
        (Some(old),Some(new))=>format!(" · {} → {}",short_oid(old),short_oid(new)),
        _=>String::new(),
    };
    vec![format!("[Push] {target} · {}{range} · OK",plural(before.ahead,"commit","commits"))]
}
fn parse_shortstat(value:&str)->(u32,u32,u32){
    let mut files=0;let mut insertions=0;let mut deletions=0;
    let fields:Vec<&str>=value.split_whitespace().collect();
    for pair in fields.windows(2){let count=pair[0].parse::<u32>().unwrap_or(0);match pair[1].trim_end_matches(','){"file"|"files"|"changed"=>if pair[1].starts_with("file"){files=count},word if word.starts_with("insertion")=>insertions=count,word if word.starts_with("deletion")=>deletions=count,_=>{}}}
    (files,insertions,deletions)
}
fn validate_oid(oid:&str)->Result<(),WorkspaceServiceError>{if(4..=64).contains(&oid.len())&&oid.bytes().all(|b|b.is_ascii_hexdigit()){Ok(())}else{Err(WorkspaceServiceError::invalid("invalid commit id"))}}
fn parse_history_item(raw:&str)->Option<GitHistoryItem>{let fields:Vec<&str>=raw.trim_matches(['\n','\r']).split('\x1f').collect();if fields.len()<7{return None;}Some(GitHistoryItem{oid:fields[0].into(),short_oid:fields[1].into(),subject:fields[2].into(),author:fields[3].into(),authored_at_unix_ms:fields[4].parse::<u64>().ok()?.saturating_mul(1000),decorations:fields[5].split(',').map(str::trim).filter(|v|!v.is_empty()).map(str::to_string).collect(),parent_oids:fields[6].split_whitespace().map(str::to_string).collect()})}
fn content_page(bytes:Vec<u8>,cursor:Option<String>,max_bytes:u32)->ContentPage{let start=cursor.as_deref().unwrap_or("0").parse::<usize>().unwrap_or(0).min(bytes.len());let limit=(max_bytes as usize).clamp(1,WORKSPACE_PAYLOAD_BUDGET);let mut end=start.saturating_add(limit).min(bytes.len());while end>start&&std::str::from_utf8(&bytes[start..end]).is_err(){end-=1;}let truncated=end<bytes.len();ContentPage{bytes:bytes[start..end].to_vec(),next_cursor:truncated.then(||end.to_string()),truncated}}

fn sanitize_git_error_message(raw: &str) -> String {
    let mut clean = String::with_capacity(raw.len().min(GIT_ERROR_MESSAGE_LIMIT));
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.next_if_eq(&'[').is_some() {
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) { break; }
                }
            } else {
                let _ = chars.next();
            }
            continue;
        }
        match character {
            '\r' => {
                let _ = chars.next_if_eq(&'\n');
                if clean.chars().last() != Some('\n') { clean.push('\n'); }
            }
            '\n' | '\t' => clean.push(character),
            value if value.is_control() => {}
            value => clean.push(value),
        }
    }

    let normalized = clean
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let mut message = redact_url_credentials(normalized.trim());
    if message.is_empty() { return "Git command failed".into(); }
    if message.len() > GIT_ERROR_MESSAGE_LIMIT {
        let suffix = '…';
        let mut end = GIT_ERROR_MESSAGE_LIMIT.saturating_sub(suffix.len_utf8());
        while !message.is_char_boundary(end) { end = end.saturating_sub(1); }
        message.truncate(end);
        while message.ends_with(char::is_whitespace) { message.pop(); }
        message.push(suffix);
    }
    message
}

fn redact_url_credentials(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative_scheme) = input[cursor..].find("://") {
        let authority_start = cursor + relative_scheme + 3;
        output.push_str(&input[cursor..authority_start]);
        let authority_end = input[authority_start..]
            .char_indices()
            .find_map(|(index, character)| {
                (character == '/' || character == '?' || character == '#' || character.is_whitespace() || character == '\'' || character == '"')
                    .then_some(authority_start + index)
            })
            .unwrap_or(input.len());
        let authority = &input[authority_start..authority_end];
        if let Some(at) = authority.rfind('@') {
            output.push_str("[redacted]@");
            output.push_str(&authority[at + 1..]);
        } else {
            output.push_str(authority);
        }
        cursor = authority_end;
    }
    output.push_str(&input[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_git_executable_has_a_stable_workspace_error_code() {
        let error = map_git_command_error(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "git executable missing",
        ));
        assert_eq!(error.code(), WorkspaceErrorCode::GitNotInstalled);
        let wire: relaycat_protocol::WorkspaceError = error.into();
        assert_eq!(wire.message, "git is not installed");
        assert!(!wire.retryable);
    }

    #[test]
    fn git_error_message_is_sanitized_redacted_and_bounded() {
        let raw = format!(
            "\u{1b}[31mfatal:\u{1b}[0m unable to access 'https://alice:secret-token@example.com/repo'\r\n{}",
            "remote hook output\n".repeat(400)
        );
        let message = sanitize_git_error_message(&raw);
        assert!(message.starts_with("fatal: unable to access 'https://[redacted]@example.com/repo'"));
        assert!(!message.contains("secret-token"));
        assert!(!message.contains('\u{1b}'));
        assert!(message.len() <= GIT_ERROR_MESSAGE_LIMIT);
        assert!(message.ends_with('…'));
    }
}
