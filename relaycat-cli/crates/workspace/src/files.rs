use crate::{ProjectRoot, WorkspaceServiceError};
use relaycat_protocol::{
    DirectoryEntry, DirectoryPage, FilePreview, ImageVariant, WORKSPACE_DIRECTORY_ENTRY_LIMIT,
    WORKSPACE_DIRECTORY_PAGE_SIZE,
};
use std::{cmp::Ordering, fs, path::Path};

pub const TEXT_PREVIEW_LIMIT: u64 = 512 * 1024;
pub const IMAGE_PREVIEW_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FileService { root: ProjectRoot }

impl FileService {
    pub fn new(root: ProjectRoot) -> Self { Self { root } }

    pub fn list(&self, path: &str, offset: u16, limit: u16) -> Result<DirectoryPage, WorkspaceServiceError> {
        let directory = self.root.resolve(path)?;
        if !directory.is_dir() { return Err(WorkspaceServiceError::invalid("path is not a directory")); }
        let mut entries = Vec::new();
        for item in fs::read_dir(&directory).map_err(WorkspaceServiceError::io)? {
            let item = item.map_err(WorkspaceServiceError::io)?;
            let resolved = match item.path().canonicalize() { Ok(value) if value.starts_with(self.root.path()) => value, _ => continue };
            let metadata = fs::metadata(&resolved).map_err(WorkspaceServiceError::io)?;
            let name = item.file_name().to_string_lossy().into_owned();
            let relative = resolved.strip_prefix(self.root.path()).map_err(|_| WorkspaceServiceError::outside_project())?;
            entries.push(DirectoryEntry {
                name,
                path: slash_path(relative),
                is_directory: metadata.is_dir(),
                size: if metadata.is_file() { metadata.len() } else { 0 },
            });
        }
        entries.sort_by(|left, right| match (left.is_directory, right.is_directory) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()).then_with(|| left.name.cmp(&right.name)),
        });
        let capped = entries.len() > WORKSPACE_DIRECTORY_ENTRY_LIMIT as usize;
        entries.truncate(WORKSPACE_DIRECTORY_ENTRY_LIMIT as usize);
        let start = usize::from(offset).min(entries.len());
        let requested = usize::from(limit.clamp(1, WORKSPACE_DIRECTORY_PAGE_SIZE));
        let end = start.saturating_add(requested).min(entries.len());
        let has_more = end < entries.len();
        Ok(DirectoryPage {
            path: path.to_string(),
            entries: entries[start..end].to_vec(),
            next_offset: has_more.then_some(end as u16),
            has_more,
            capped,
        })
    }

    pub fn read(&self, path: &str, max_bytes: u32, _variant: ImageVariant) -> Result<FilePreview, WorkspaceServiceError> {
        let resolved = self.root.resolve(path)?;
        let metadata = fs::metadata(&resolved).map_err(WorkspaceServiceError::io)?;
        if !metadata.is_file() { return Err(WorkspaceServiceError::invalid("path is not a file")); }
        let image = image_kind(&resolved);
        let ceiling = if image.is_some() { IMAGE_PREVIEW_LIMIT } else { TEXT_PREVIEW_LIMIT };
        let limit = u64::from(max_bytes.max(1)).min(ceiling);
        if metadata.len() > limit {
            return Ok(FilePreview::TooLarge { path: path.to_string(), size: metadata.len(), limit });
        }
        let bytes = fs::read(&resolved).map_err(WorkspaceServiceError::io)?;
        if let Some(mime) = image {
            let (width, height) = image_dimensions(mime, &bytes).unwrap_or((0, 0));
            return Ok(FilePreview::Image {
                path: path.to_string(), mime: mime.to_string(), width, height, bytes, truncated: false,
            });
        }
        match String::from_utf8(bytes) {
            Ok(content) if !content.contains('\0') => Ok(FilePreview::Text {
                path: path.to_string(), language: language_for(&resolved).to_string(), content, truncated: false,
            }),
            _ => Ok(FilePreview::Binary { path: path.to_string(), size: metadata.len() }),
        }
    }
}

fn slash_path(path: &Path) -> String { path.to_string_lossy().replace('\\', "/") }

pub fn language_for(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "rs" => "rust", "swift" => "swift", "kt" | "kts" => "kotlin",
        "js" | "jsx" => "javascript", "ts" | "tsx" => "typescript", "json" => "json",
        "toml" => "toml", "yaml" | "yml" => "yaml", "md" => "markdown",
        "sh" | "zsh" | "bash" => "shell", "css" => "css", "html" | "htm" => "html",
        "py" => "python", "go" => "go", "c" | "h" => "c", "cpp" | "cc" | "hpp" => "cpp",
        _ => "plain_text",
    }
}

fn image_kind(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"), "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"), "webp" => Some("image/webp"), _ => None,
    }
}

fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
            Some((u32::from_be_bytes(bytes[16..20].try_into().ok()?), u32::from_be_bytes(bytes[20..24].try_into().ok()?)))
        }
        "image/gif" if bytes.len() >= 10 => Some((u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32, u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32)),
        "image/webp" if bytes.len() >= 30 && &bytes[12..16] == b"VP8X" => {
            let width = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
            let height = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
            Some((width, height))
        }
        "image/jpeg" => jpeg_dimensions(bytes),
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) { return None; }
    let mut index = 2;
    while index + 8 < bytes.len() {
        if bytes[index] != 0xff { index += 1; continue; }
        let marker = bytes[index + 1];
        index += 2;
        if matches!(marker, 0xd8 | 0xd9) { continue; }
        let length = u16::from_be_bytes(bytes.get(index..index + 2)?.try_into().ok()?) as usize;
        if length < 2 || index + length > bytes.len() { return None; }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let height = u16::from_be_bytes(bytes[index + 3..index + 5].try_into().ok()?) as u32;
            let width = u16::from_be_bytes(bytes[index + 5..index + 7].try_into().ok()?) as u32;
            return Some((width, height));
        }
        index += length;
    }
    None
}
