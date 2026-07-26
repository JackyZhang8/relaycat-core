use std::path::{Path, PathBuf};

use crate::git::runner::git_output;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use relaycat_protocol::{DirectoryPage, FilePreview, ImageVariant};
use relaycat_workspace::{FileService, ProjectRoot, IMAGE_PREVIEW_LIMIT};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceEntryDto {
    pub name: String,
    pub relative_path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified_unix_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceEntriesPageDto {
    pub entries: Vec<WorkspaceEntryDto>,
    pub has_more: bool,
    pub capped: bool,
}

impl From<DirectoryPage> for WorkspaceEntriesPageDto {
    fn from(page: DirectoryPage) -> Self {
        Self {
            entries: page
                .entries
                .into_iter()
                .map(|entry| WorkspaceEntryDto {
                    name: entry.name,
                    relative_path: entry.path,
                    is_dir: entry.is_directory,
                    size_bytes: entry.size,
                    modified_unix_seconds: entry.modified_unix_seconds,
                })
                .collect(),
            has_more: page.has_more,
            capped: page.capped,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Text,
    Image,
    Archive,
    Binary,
    TooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FilePreviewDto {
    pub kind: PreviewKind,
    pub content: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitChangeDto {
    pub path: String,
    pub index_status: Option<String>,
    pub worktree_status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitStatusDto {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub changes: Vec<GitChangeDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitCommitDto {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub refs: Vec<String>,
    pub author: String,
    pub date: String,
    pub subject: String,
}

const MAX_DIRECTORY_ENTRIES: usize = 1000;
const DIRECTORY_PAGE_SIZE: usize = 100;
const MAX_FILE_PREVIEW_BYTES: u64 = 512 * 1024;
const MAX_DIFF_BYTES: usize = 1024 * 1024;

fn project_root(project: &Path) -> Result<PathBuf, String> {
    ProjectRoot::open(project)
        .map(|root| root.path().to_path_buf())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn resolve_project_path(project: &Path, relative: &str) -> Result<PathBuf, String> {
    ProjectRoot::open(project)
        .and_then(|root| root.resolve(relative))
        .map_err(|error| error.to_string())
}

fn relative_path_string(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
fn list_workspace_entries_impl(
    project: &str,
    relative_path: &str,
) -> Result<Vec<WorkspaceEntryDto>, String> {
    Ok(list_workspace_entries_page_impl(project, relative_path, 0, DIRECTORY_PAGE_SIZE)?.entries)
}

fn list_workspace_entries_page_impl(
    project: &str,
    relative_path: &str,
    offset: usize,
    limit: usize,
) -> Result<WorkspaceEntriesPageDto, String> {
    let root = ProjectRoot::open(project).map_err(|error| error.to_string())?;
    let page = FileService::new(root)
        .list(
            relative_path,
            offset.min(MAX_DIRECTORY_ENTRIES) as u16,
            limit.clamp(1, DIRECTORY_PAGE_SIZE) as u16,
        )
        .map_err(|error| error.to_string())?;
    Ok(page.into())
}

fn read_workspace_file_impl(
    project: &str,
    relative_path: &str,
    max_bytes: u64,
) -> Result<FilePreviewDto, String> {
    let image_extension = Path::new(relative_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"));
    let requested = if image_extension.is_some() {
        IMAGE_PREVIEW_LIMIT
    } else {
        max_bytes.clamp(1, MAX_FILE_PREVIEW_BYTES)
    };
    let preview = FileService::new(ProjectRoot::open(project).map_err(|error| error.to_string())?)
        .read(relative_path, requested as u32, ImageVariant::Original)
        .map_err(|error| error.to_string())?;
    Ok(file_preview_dto(preview))
}

fn file_preview_dto(preview: FilePreview) -> FilePreviewDto {
    match preview {
        FilePreview::Text { content, .. } => FilePreviewDto {
            size_bytes: content.len() as u64,
            kind: PreviewKind::Text,
            content,
            mime_type: None,
        },
        FilePreview::Image { mime, bytes, .. } => FilePreviewDto {
            size_bytes: bytes.len() as u64,
            kind: PreviewKind::Image,
            content: format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            mime_type: Some(mime),
        },
        FilePreview::Archive {
            format,
            entries,
            has_more,
            ..
        } => {
            let mut lines = entries
                .into_iter()
                .map(|entry| {
                    let metadata = if entry.is_directory {
                        "DIR".to_string()
                    } else {
                        format!("{} B", entry.size)
                    };
                    format!("{metadata}\t{}", entry.path)
                })
                .collect::<Vec<_>>();
            if has_more {
                lines.push("… Showing the first 50 items".into());
            }
            FilePreviewDto {
                kind: PreviewKind::Archive,
                content: lines.join("\n"),
                mime_type: Some(format),
                size_bytes: 0,
            }
        }
        FilePreview::Binary { size, .. } => FilePreviewDto {
            kind: PreviewKind::Binary,
            content: String::new(),
            mime_type: None,
            size_bytes: size,
        },
        FilePreview::TooLarge { size, .. } => FilePreviewDto {
            kind: PreviewKind::TooLarge,
            content: String::new(),
            mime_type: None,
            size_bytes: size,
        },
    }
}

fn parse_porcelain_v1_z(raw: &[u8]) -> Result<Vec<GitChangeDto>, String> {
    let records = raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 || record[2] != b' ' {
            return Err("invalid git status output".into());
        }
        let index_code = record[0] as char;
        let worktree_code = record[1] as char;
        if index_code == '!' && worktree_code == '!' {
            index += 1;
            continue;
        }
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let (index_status, worktree_status) = if index_code == '?' && worktree_code == '?' {
            (None, Some("?".to_string()))
        } else {
            (
                (index_code != ' ').then(|| index_code.to_string()),
                (worktree_code != ' ').then(|| worktree_code.to_string()),
            )
        };
        changes.push(GitChangeDto {
            path,
            index_status,
            worktree_status,
        });
        index += 1;
        if matches!(index_code, 'R' | 'C') || matches!(worktree_code, 'R' | 'C') {
            if index >= records.len() {
                return Err("missing original path for renamed git entry".into());
            }
            index += 1;
        }
    }
    Ok(changes)
}

fn parse_git_log(raw: &[u8]) -> Result<Vec<GitCommitDto>, String> {
    raw.split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let fields = record.split(|byte| *byte == 0x1f).collect::<Vec<_>>();
            if fields.len() != 7 {
                return Err("invalid git history output".to_string());
            }
            Ok(GitCommitDto {
                hash: String::from_utf8_lossy(fields[0]).into_owned(),
                short_hash: String::from_utf8_lossy(fields[1]).into_owned(),
                parents: String::from_utf8_lossy(fields[2])
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                refs: String::from_utf8_lossy(fields[3])
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect(),
                author: String::from_utf8_lossy(fields[4]).into_owned(),
                date: String::from_utf8_lossy(fields[5]).into_owned(),
                subject: String::from_utf8_lossy(fields[6]).into_owned(),
            })
        })
        .collect()
}

fn valid_commit_id(commit: &str) -> bool {
    (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalized_history_page(skip: u32, limit: u32) -> (u32, u32) {
    (skip, limit.clamp(1, 100))
}

fn git_workspace_status_impl(project: &str) -> Result<GitStatusDto, String> {
    let root = project_root(Path::new(project))?;
    let probe = git_output(&root, &["rev-parse", "--show-toplevel"])?;
    if !probe.status.success() {
        return Ok(GitStatusDto {
            is_repo: false,
            branch: None,
            changes: Vec::new(),
        });
    }
    let repo_root = PathBuf::from(String::from_utf8_lossy(&probe.stdout).trim())
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let branch_output = git_output(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch_output.status.success().then(|| {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    });
    let status = git_output(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    if !status.status.success() {
        return Err(String::from_utf8_lossy(&status.stderr).trim().to_string());
    }
    let mut changes = parse_porcelain_v1_z(&status.stdout)?;
    for change in &mut changes {
        change.path = relative_path_string(&root, &repo_root.join(&change.path))?;
    }
    Ok(GitStatusDto {
        is_repo: true,
        branch: branch.filter(|branch| !branch.is_empty()),
        changes,
    })
}

fn git_workspace_diff_impl(
    project: &str,
    relative_path: &str,
    staged: bool,
) -> Result<FilePreviewDto, String> {
    let root = project_root(Path::new(project))?;
    ProjectRoot::open(&root)
        .and_then(|project| project.lexical_path(relative_path))
        .map_err(|error| error.to_string())?;
    let mut args = vec!["diff", "--no-ext-diff", "--no-color"];
    if staged {
        args.push("--cached");
    }
    args.extend(["--", relative_path]);
    let output = git_output(&root, &args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let bytes = output.stdout;
    if bytes.len() > MAX_DIFF_BYTES {
        return Ok(FilePreviewDto {
            kind: PreviewKind::TooLarge,
            content: String::new(),
            mime_type: None,
            size_bytes: bytes.len() as u64,
        });
    }
    Ok(FilePreviewDto {
        kind: PreviewKind::Text,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        mime_type: None,
        size_bytes: bytes.len() as u64,
    })
}

fn git_workspace_history_impl(
    project: &str,
    skip: u32,
    limit: u32,
    query: Option<&str>,
    author: Option<&str>,
    reference: Option<&str>,
) -> Result<Vec<GitCommitDto>, String> {
    let root = project_root(Path::new(project))?;
    if !git_output(&root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success()
    {
        return Ok(Vec::new());
    }
    let (skip, limit) = normalized_history_page(skip, limit);
    let skip_arg = format!("--skip={skip}");
    let limit_arg = format!("-n{limit}");
    let mut args = vec![
        "log".to_string(),
        skip_arg,
        limit_arg,
        "-z".into(),
        "--date=iso-strict".into(),
        "--pretty=format:%H%x1f%h%x1f%P%x1f%D%x1f%an%x1f%aI%x1f%s".into(),
    ];
    if let Some(value) = query.filter(|value| !value.trim().is_empty()) {
        args.push(format!("--grep={}", value.trim()));
    }
    if let Some(value) = author.filter(|value| !value.trim().is_empty()) {
        args.push(format!("--author={}", value.trim()));
    }
    if let Some(value) = reference.filter(|value| !value.trim().is_empty()) {
        let value = value.trim();
        if value.starts_with('-') {
            return Err("invalid history reference".into());
        }
        let commit = format!("{value}^{{commit}}");
        if !git_output(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &commit,
            ],
        )?
        .status
        .success()
        {
            return Err("invalid history reference".into());
        }
        args.push(value.to_string());
    }
    args.extend(["--".into(), ".".into()]);
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = git_output(&root, &arg_refs)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_git_log(&output.stdout)
}

fn git_workspace_commit_diff_impl(project: &str, commit: &str) -> Result<FilePreviewDto, String> {
    if !valid_commit_id(commit) {
        return Err("invalid commit id".into());
    }
    let root = project_root(Path::new(project))?;
    let output = git_output(
        &root,
        &[
            "show",
            "--no-ext-diff",
            "--no-color",
            "--format=fuller",
            commit,
            "--",
            ".",
        ],
    )?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let bytes = output.stdout;
    if bytes.len() > MAX_DIFF_BYTES {
        return Ok(FilePreviewDto {
            kind: PreviewKind::TooLarge,
            content: String::new(),
            mime_type: None,
            size_bytes: bytes.len() as u64,
        });
    }
    Ok(FilePreviewDto {
        kind: PreviewKind::Text,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        mime_type: None,
        size_bytes: bytes.len() as u64,
    })
}

#[tauri::command]
pub async fn list_workspace_entries(
    project: String,
    relative_path: String,
    offset: usize,
    limit: usize,
) -> Result<WorkspaceEntriesPageDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        list_workspace_entries_page_impl(&project, &relative_path, offset, limit)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn read_workspace_file(
    project: String,
    relative_path: String,
    max_bytes: u64,
) -> Result<FilePreviewDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        read_workspace_file_impl(&project, &relative_path, max_bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_workspace_status(project: String) -> Result<GitStatusDto, String> {
    tauri::async_runtime::spawn_blocking(move || git_workspace_status_impl(&project))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_workspace_diff(
    project: String,
    relative_path: String,
    staged: bool,
) -> Result<FilePreviewDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        git_workspace_diff_impl(&project, &relative_path, staged)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_workspace_history(
    project: String,
    skip: u32,
    limit: u32,
    query: Option<String>,
    author: Option<String>,
    reference: Option<String>,
) -> Result<Vec<GitCommitDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        git_workspace_history_impl(
            &project,
            skip,
            limit,
            query.as_deref(),
            author.as_deref(),
            reference.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_workspace_commit_diff(
    project: String,
    commit: String,
) -> Result<FilePreviewDto, String> {
    tauri::async_runtime::spawn_blocking(move || git_workspace_commit_diff_impl(&project, &commit))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn shared_directory_page_preserves_gui_contract() {
        let root = temp_project("shared-page");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let shared = relaycat_workspace::FileService::new(ProjectRoot::open(&root).unwrap())
            .list("", 0, 100)
            .unwrap();

        let dto = WorkspaceEntriesPageDto::from(shared.clone());
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(dto.entries.len(), shared.entries.len());
        assert_eq!(dto.entries[0].relative_path, "main.rs");
        assert_eq!(dto.has_more, shared.has_more);
        assert_eq!(dto.capped, shared.capped);
    }

    fn temp_project(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "relaycat-workspace-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn rejects_parent_directory_traversal() {
        let root = temp_project("traversal");
        let result = resolve_project_path(&root, "../secret");
        fs::remove_dir_all(&root).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn lists_directories_before_files_and_skips_generated_directories() {
        let root = temp_project("listing");
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();

        let entries = list_workspace_entries_impl(root.to_str().unwrap(), "").unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.is_dir))
                .collect::<Vec<_>>(),
            vec![("src", true), ("README.md", false)]
        );
    }

    #[test]
    fn limits_the_initial_directory_page_to_one_hundred_entries() {
        let root = temp_project("listing-page");
        for index in 0..150 {
            fs::write(root.join(format!("file-{index:03}.txt")), "x").unwrap();
        }

        let entries = list_workspace_entries_impl(root.to_str().unwrap(), "").unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(entries.len(), 100);
        assert_eq!(entries.first().unwrap().name, "file-000.txt");
        assert_eq!(entries.last().unwrap().name, "file-099.txt");
    }

    #[test]
    fn paginates_directory_entries_and_caps_the_result_at_one_thousand() {
        let root = temp_project("listing-cap");
        for index in 0..1_100 {
            fs::write(root.join(format!("file-{index:04}.txt")), "x").unwrap();
        }

        let second_page = list_workspace_entries_page_impl(
            root.to_str().unwrap(),
            "",
            DIRECTORY_PAGE_SIZE,
            DIRECTORY_PAGE_SIZE,
        )
        .unwrap();
        let final_page = list_workspace_entries_page_impl(
            root.to_str().unwrap(),
            "",
            MAX_DIRECTORY_ENTRIES - DIRECTORY_PAGE_SIZE,
            DIRECTORY_PAGE_SIZE,
        )
        .unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(second_page.entries.len(), 100);
        assert_eq!(second_page.entries.first().unwrap().name, "file-0100.txt");
        assert_eq!(second_page.entries.last().unwrap().name, "file-0199.txt");
        assert!(second_page.has_more);
        assert!(second_page.capped);
        assert_eq!(final_page.entries.len(), 100);
        assert_eq!(final_page.entries.first().unwrap().name, "file-0900.txt");
        assert_eq!(final_page.entries.last().unwrap().name, "file-0999.txt");
        assert!(!final_page.has_more);
        assert!(final_page.capped);
    }

    #[test]
    fn classifies_text_binary_image_and_oversized_files() {
        let root = temp_project("preview");
        fs::write(root.join("text.txt"), "hello").unwrap();
        fs::write(root.join("large.txt"), "abcdef").unwrap();
        fs::write(root.join("binary.bin"), b"a\0b").unwrap();
        fs::write(root.join("pixel.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();

        let text = read_workspace_file_impl(root.to_str().unwrap(), "text.txt", 100).unwrap();
        let large = read_workspace_file_impl(root.to_str().unwrap(), "large.txt", 4).unwrap();
        let binary = read_workspace_file_impl(root.to_str().unwrap(), "binary.bin", 100).unwrap();
        let image = read_workspace_file_impl(root.to_str().unwrap(), "pixel.png", 4).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(text.kind, PreviewKind::Text);
        assert_eq!(text.content, "hello");
        assert_eq!(large.kind, PreviewKind::TooLarge);
        assert!(large.content.is_empty());
        assert_eq!(binary.kind, PreviewKind::Binary);
        assert!(binary.content.is_empty());
        assert_eq!(image.kind, PreviewKind::Image);
        assert_eq!(image.mime_type.as_deref(), Some("image/png"));
        assert!(image.content.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn converts_archive_preview_to_read_only_text_listing() {
        let preview = FilePreview::Archive {
            path: "bundle.zip".into(),
            format: "zip".into(),
            entries: vec![
                relaycat_protocol::ArchiveEntry {
                    path: "src/".into(),
                    is_directory: true,
                    size: 0,
                    modified_unix_seconds: None,
                },
                relaycat_protocol::ArchiveEntry {
                    path: "src/main.rs".into(),
                    is_directory: false,
                    size: 42,
                    modified_unix_seconds: Some(1_700_000_000),
                },
            ],
            has_more: true,
        };

        let dto = file_preview_dto(preview);

        assert_eq!(dto.kind, PreviewKind::Archive);
        assert_eq!(dto.mime_type.as_deref(), Some("zip"));
        assert_eq!(dto.size_bytes, 0);
        assert_eq!(
            dto.content,
            "DIR\tsrc/\n42 B\tsrc/main.rs\n… Showing the first 50 items"
        );
    }

    #[test]
    fn parses_staged_unstaged_untracked_and_renamed_status() {
        let raw = b"M  src/a.rs\0 M src/b.rs\0?? notes.txt\0R  src/new.rs\0src/old.rs\0";
        let changes = parse_porcelain_v1_z(raw).unwrap();

        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].index_status.as_deref(), Some("M"));
        assert_eq!(changes[1].worktree_status.as_deref(), Some("M"));
        assert_eq!(changes[2].worktree_status.as_deref(), Some("?"));
        assert_eq!(changes[3].path, "src/new.rs");
        assert_eq!(changes[3].index_status.as_deref(), Some("R"));
    }

    #[test]
    fn parses_git_history_records_and_validates_commit_ids() {
        let raw = b"0123456789abcdef\x1f0123456\x1fabcdef0\x1fHEAD -> main, tag: v1\x1fAlice\x1f2026-07-24T08:00:00Z\x1fAdd history\0";
        let commits = parse_git_log(raw).unwrap();

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short_hash, "0123456");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].parents, vec!["abcdef0"]);
        assert_eq!(commits[0].refs, vec!["HEAD -> main", "tag: v1"]);
        assert_eq!(commits[0].subject, "Add history");
        assert!(valid_commit_id("0123456"));
        assert!(!valid_commit_id("--help"));
    }

    #[test]
    fn normalizes_git_history_pagination() {
        assert_eq!(normalized_history_page(0, 20), (0, 20));
        assert_eq!(normalized_history_page(40, 500), (40, 100));
    }

    #[test]
    fn rejects_history_references_that_trim_to_options() {
        let root = temp_project("history-option");
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "RelayCat Test"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "relaycat@example.test"])
            .current_dir(&root)
            .status()
            .unwrap();
        fs::write(root.join("file.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "--quiet", "-m", "initial"])
            .current_dir(&root)
            .status()
            .unwrap();

        let result =
            git_workspace_history_impl(root.to_str().unwrap(), 0, 20, None, None, Some(" --all"));
        fs::remove_dir_all(&root).unwrap();
        assert!(result.unwrap_err().contains("invalid history reference"));
    }

    #[test]
    fn git_status_is_scoped_to_the_session_project_directory() {
        let root = temp_project("git-scope");
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap();
        fs::write(root.join("outside.txt"), "outside").unwrap();
        fs::write(project.join("inside.txt"), "inside").unwrap();

        let status = git_workspace_status_impl(project.to_str().unwrap()).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            status
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            vec!["inside.txt"]
        );
    }
}
