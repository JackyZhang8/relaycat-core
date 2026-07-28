use serde::{Deserialize, Serialize};

pub const WORKSPACE_PAYLOAD_BUDGET: usize = 768 * 1024;
pub const WORKSPACE_DIRECTORY_PAGE_SIZE: u16 = 100;
pub const WORKSPACE_DIRECTORY_ENTRY_LIMIT: u16 = 1000;
pub const WORKSPACE_GIT_HISTORY_PAGE_SIZE: u16 = 20;
pub const WORKSPACE_APP_SHELL_LIMIT: u8 = 3;
pub const WORKSPACE_ARCHIVE_ENTRY_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRequestEnvelope { pub request_id: String, pub project_id: String, pub deadline_unix_ms: u64, pub idempotency_key: Option<String>, pub operation: WorkspaceRequest }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceResponseEnvelope { pub request_id: String, pub project_id: String, pub result: std::result::Result<WorkspaceResponse, WorkspaceError> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEventEnvelope { pub project_id: String, pub event_seq: u64, pub event: WorkspaceEvent }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum WorkspaceRequest {
    Capabilities, Cancel { target_request_id: String },
    ListDirectory { path: String, offset: u16, limit: u16 },
    SearchFiles { query: String, limit: u16 },
    ReadFile { path: String, max_bytes: u32, image_variant: ImageVariant },
    GitSummary, GitStatus,
    GitDiff { target: GitDiffTarget, cursor: Option<String>, max_bytes: u32 },
    GitHistory { query: String, author: String, reference: String, cursor: Option<String>, limit: u16 },
    GitCommitDetail { oid: String, cursor: Option<String>, max_bytes: u32 }, GitListRefs,
    GitStage { paths: Vec<String> }, GitUnstage { paths: Vec<String> }, GitDiscard { paths: Vec<String> },
    GitApplyPatch { patch: String, reverse: bool, cached: bool }, GitCheckout { reference: String },
    GitCreateBranch { name: String, start_point: String }, GitRenameBranch { name: String },
    GitDeleteBranch { name: String, force: bool }, GitCreateTag { name: String, target: String },
    GitCommit { message: String, amend: bool, push_after: bool }, GitRemote { kind: GitRemoteOperation },
    GitCommitAction { kind: GitCommitAction, oid: String }, GitReset { oid: String, mode: GitResetMode },
    ShellList, ShellCreate { cols: u16, rows: u16 },
    ShellInput { shell_id: String, input_seq: u64, bytes: Vec<u8> },
    ShellResize { shell_id: String, cols: u16, rows: u16 },
    ShellSnapshot { shell_id: String, after_output_seq: u64 }, ShellClose { shell_id: String }, ShellCloseAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum WorkspaceResponse {
    Capabilities(WorkspaceCapabilities), Directory(DirectoryPage), FileSearch(FileSearchPage), File(FilePreview),
    GitSummary(GitSummary), GitStatus(GitStatus), GitDiff(ContentPage), GitHistory(GitHistoryPage),
    GitCommitDetail(GitCommitDetail), GitRefs(Vec<GitRef>), GitMutation(GitMutationResult),
    ShellList(Vec<ShellDescriptor>), ShellCreated(ShellSnapshot), ShellSnapshot(ShellSnapshot), Ack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum WorkspaceEvent {
    GitProgress(GitProgressEvent), GitStatusChanged { fingerprint: String },
    ShellOutput { shell_id: String, output_seq: u64, bytes: Vec<u8> }, ShellExit { shell_id: String, code: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceErrorCode { Unsupported, InvalidRequest, PermissionDenied, PathOutsideProject, NotFound, TooLarge, Binary, NotGitRepository, Conflict, Timeout, Busy, Cancelled, Internal }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceError { pub code: WorkspaceErrorCode, pub message: String, pub retryable: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCapabilities { pub files: bool, pub git_read: bool, pub git_write: bool, pub shell: bool, pub directory_page_size: u16, pub directory_entry_limit: u16, pub git_history_page_size: u16, pub text_preview_limit: u32, pub image_preview_limit: u32, pub shell_limit: u8 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryPage { pub path: String, pub entries: Vec<DirectoryEntry>, pub next_offset: Option<u16>, pub has_more: bool, pub capped: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSearchPage { pub entries: Vec<DirectoryEntry>, pub capped: bool }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageVariant { Thumbnail, Original }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FilePreview {
    Text { path: String, language: String, content: String, truncated: bool },
    Image { path: String, mime: String, width: u32, height: u32, bytes: Vec<u8>, truncated: bool },
    Archive { path: String, format: String, entries: Vec<ArchiveEntry>, has_more: bool },
    Database { path: String, format: String, size: u64, objects: Vec<DatabaseObject>, has_more: bool },
    Binary { path: String, size: u64 }, TooLarge { path: String, size: u64, limit: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseObject {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub columns: Vec<DatabaseColumn>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseColumn {
    pub name: String,
    pub declared_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSummary { pub branch: String, pub upstream: Option<String>, pub ahead: u32, pub behind: u32, pub staged_count: u32, pub unstaged_count: u32, pub untracked_count: u32, pub fingerprint: String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitChange { pub path: String, pub old_path: Option<String>, pub status: String, pub staged: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus { pub branch: String, pub changes: Vec<GitChange>, pub fingerprint: String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GitDiffTarget { WorkingTree, Staged, Path { path: String, staged: bool }, Commit { oid: String } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPage { pub bytes: Vec<u8>, pub next_cursor: Option<String>, pub truncated: bool }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryItem { pub oid: String, pub short_oid: String, pub subject: String, pub author: String, pub authored_at_unix_ms: u64, pub decorations: Vec<String>, pub parent_oids: Vec<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryPage { pub items: Vec<GitHistoryItem>, pub next_cursor: Option<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitDetail { pub item: GitHistoryItem, pub body: String, pub changed_paths: Vec<GitChange>, pub diff: ContentPage }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRef { pub name: String, pub short_name: String, pub kind: String, pub current: bool, pub oid: String }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRemoteOperation { Fetch, Pull, Push }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitAction { Revert, CherryPick }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitResetMode { Soft, Mixed, Hard }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitMutationResult { pub operation_id: String, pub summary: GitSummary }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitProgressEvent { pub operation_id: String, pub phase: String, pub message: String, pub percent: Option<u8>, pub completed: bool, pub success: Option<bool> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellDescriptor { pub shell_id: String, pub title: String, pub cols: u16, pub rows: u16, pub last_output_seq: u64, pub exited: bool, pub exit_code: Option<i32> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellSnapshot { pub descriptor: ShellDescriptor, pub first_output_seq: u64, pub last_output_seq: u64, pub bytes: Vec<u8>, pub complete_screen: bool }
