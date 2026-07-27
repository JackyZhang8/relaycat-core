use crate::{ProjectRoot, WorkspaceServiceError};
use relaycat_protocol::{
    DirectoryEntry, DirectoryPage, FilePreview, ImageVariant, WORKSPACE_DIRECTORY_ENTRY_LIMIT,
    WORKSPACE_DIRECTORY_PAGE_SIZE,
};
use std::{cmp::Ordering, fs, path::Path};

pub const TEXT_PREVIEW_LIMIT: u64 = 512 * 1024;
pub const IMAGE_PREVIEW_LIMIT: u64 = 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePreviewLimits {
    pub text: u64,
    pub image: u64,
    pub database: u64,
}

pub const MOBILE_FILE_PREVIEW_LIMITS: FilePreviewLimits = FilePreviewLimits {
    text: TEXT_PREVIEW_LIMIT,
    image: IMAGE_PREVIEW_LIMIT,
    database: crate::database::DATABASE_PREVIEW_SOURCE_LIMIT,
};
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
];

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
            let file_name = item.file_name();
            let name = file_name.to_string_lossy().into_owned();
            if metadata.is_dir() && IGNORED_DIRECTORIES.contains(&name.as_str()) { continue; }
            let logical_path = Path::new(path).join(file_name);
            entries.push(DirectoryEntry {
                name,
                path: slash_path(&logical_path),
                is_directory: metadata.is_dir(),
                size: if metadata.is_file() { metadata.len() } else { 0 },
                modified_unix_seconds: metadata.modified().ok()
                    .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|value| value.as_secs()),
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

    pub fn read(&self, path: &str, max_bytes: u32, variant: ImageVariant) -> Result<FilePreview, WorkspaceServiceError> {
        self.read_with_limits(path, u64::from(max_bytes), variant, MOBILE_FILE_PREVIEW_LIMITS)
    }

    pub fn read_with_limits(
        &self,
        path: &str,
        max_bytes: u64,
        variant: ImageVariant,
        limits: FilePreviewLimits,
    ) -> Result<FilePreview, WorkspaceServiceError> {
        let resolved = self.root.resolve(path)?;
        let metadata = fs::metadata(&resolved).map_err(WorkspaceServiceError::io)?;
        if !metadata.is_file() { return Err(WorkspaceServiceError::invalid("path is not a file")); }
        if let Some(preview) = crate::database::preview(
            &resolved,
            path,
            metadata.len(),
            limits.database,
        ) {
            return Ok(preview);
        }
        if is_known_unsupported_binary(&resolved) {
            return Ok(FilePreview::Binary { path: path.to_string(), size: metadata.len() });
        }
        if let Some(preview) = crate::archive::preview(&resolved, path, metadata.len())? { return Ok(preview); }
        let image = image_kind(&resolved);
        let ceiling = if image.is_some() { limits.image } else { limits.text };
        let limit = max_bytes.max(1).min(ceiling);
        if metadata.len() > limit {
            return Ok(FilePreview::TooLarge { path: path.to_string(), size: metadata.len(), limit });
        }
        let bytes = fs::read(&resolved).map_err(WorkspaceServiceError::io)?;
        if let Some(mime) = image {
            let (width, height) = image_dimensions(mime, &bytes).unwrap_or((0, 0));
            if variant == ImageVariant::Thumbnail && (width > 512 || height > 512) {
                if let Some((thumbnail, thumbnail_width, thumbnail_height)) = thumbnail_jpeg(&bytes) {
                    return Ok(FilePreview::Image {
                        path: path.to_string(), mime: "image/jpeg".to_string(),
                        width: thumbnail_width, height: thumbnail_height,
                        bytes: thumbnail, truncated: false,
                    });
                }
            }
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

fn is_known_unsupported_binary(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "parquet" | "arrow" | "feather" | "avro" | "orc"
            | "sqlite" | "sqlite3" | "db" | "db3"
            | "wasm" | "class" | "dex" | "o" | "obj" | "a" | "lib"
            | "so" | "dylib" | "dll" | "exe" | "pdb"
    )
}

fn thumbnail_jpeg(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let resized = decoded.resize(512, 512, image::imageops::FilterType::Lanczos3);
    let width = resized.width();
    let height = resized.height();
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 82)
        .encode_image(&resized)
        .ok()?;
    Some((output, width, height))
}

#[cfg(windows)]
fn slash_path(path: &Path) -> String { path.to_string_lossy().replace('\\', "/") }

#[cfg(not(windows))]
fn slash_path(path: &Path) -> String { path.to_string_lossy().into_owned() }

pub fn language_for(path: &Path) -> &'static str {
    let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    match file_name.as_str() {
        "dockerfile" | "containerfile" => return "dockerfile",
        "makefile" | "gnumakefile" => return "makefile",
        "cmakelists.txt" => return "cmake",
        "jenkinsfile" => return "groovy",
        "procfile" => return "shell",
        "podfile" => return "ruby",
        "gemfile" | "rakefile" => return "ruby",
        ".env" | ".editorconfig" | ".gitignore" | ".dockerignore" => return "config",
        _ => {}
    }
    match path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "rs" => "rust", "swift" => "swift", "kt" | "kts" => "kotlin",
        "js" | "jsx" | "mjs" | "cjs" => "javascript", "ts" | "tsx" => "typescript",
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "map" => "json",
        "toml" => "toml", "yaml" | "yml" => "yaml", "md" | "markdown" => "markdown",
        "sh" | "zsh" | "bash" => "shell", "css" => "css", "html" | "htm" => "html",
        "py" => "python", "go" => "go", "c" | "h" => "c", "cpp" | "cc" | "hpp" => "cpp",
        "diff" | "patch" => "diff", "csv" | "tsv" => "csv", "svg" => "svg",
        "xml" => "xml", "scss" | "less" => "css", "cs" => "csharp", "java" => "java",
        "php" => "php", "rb" => "ruby", "m" | "mm" => "objective-c", "sql" => "sql",
        "graphql" | "gql" => "graphql", "proto" => "protobuf", "dart" => "dart",
        "lua" => "lua", "scala" => "scala", "vue" => "vue", "svelte" => "svelte",
        "tf" | "tfvars" | "hcl" => "hcl", "nix" => "nix", "gradle" | "groovy" => "groovy",
        "ini" | "conf" | "properties" | "env" | "plist" => "config",
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
