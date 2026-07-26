use crate::WorkspaceServiceError;
use flate2::read::GzDecoder;
use relaycat_protocol::{ArchiveEntry, FilePreview, WORKSPACE_ARCHIVE_ENTRY_LIMIT};
use std::{
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Component, Path},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ARCHIVE_SCAN_LIMIT: u64 = 64 * 1024 * 1024;
const ARCHIVE_PARSE_DEADLINE: Duration = Duration::from_secs(3);
pub const ARCHIVE_PREVIEW_SOURCE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
enum ArchiveKind { Zip, SevenZ, Tar, TarGz, Tgz, Gz }

impl ArchiveKind {
    fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if name.ends_with(".tar.gz") { Some(Self::TarGz) }
        else if name.ends_with(".tgz") { Some(Self::Tgz) }
        else if name.ends_with(".zip") { Some(Self::Zip) }
        else if name.ends_with(".7z") { Some(Self::SevenZ) }
        else if name.ends_with(".tar") { Some(Self::Tar) }
        else if name.ends_with(".gz") { Some(Self::Gz) }
        else { None }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Zip => "zip", Self::SevenZ => "7z", Self::Tar => "tar",
            Self::TarGz => "tar.gz", Self::Tgz => "tgz", Self::Gz => "gz",
        }
    }
}

pub fn preview(path: &Path, relative_path: &str, source_size: u64) -> Result<Option<FilePreview>, WorkspaceServiceError> {
    let Some(kind) = ArchiveKind::detect(path) else { return Ok(None) };
    if !matches!(kind, ArchiveKind::Gz) && source_size > ARCHIVE_PREVIEW_SOURCE_LIMIT {
        return Ok(Some(FilePreview::TooLarge {
            path: relative_path.to_string(),
            size: source_size,
            limit: ARCHIVE_PREVIEW_SOURCE_LIMIT,
        }));
    }
    let (entries, has_more) = match kind {
        ArchiveKind::Zip => zip_entries(path)?,
        ArchiveKind::SevenZ => sevenz_entries(path)?,
        ArchiveKind::Tar => tar_entries(BufReader::new(File::open(path).map_err(WorkspaceServiceError::io)?))?,
        ArchiveKind::TarGz | ArchiveKind::Tgz => {
            let file = File::open(path).map_err(WorkspaceServiceError::io)?;
            tar_entries(GzDecoder::new(BufReader::new(file)).take(ARCHIVE_SCAN_LIMIT))?
        }
        ArchiveKind::Gz => (vec![gzip_entry(path)?], false),
    };
    Ok(Some(FilePreview::Archive {
        path: relative_path.to_string(),
        format: kind.label().to_string(),
        entries,
        has_more,
    }))
}

fn zip_entries(path: &Path) -> Result<(Vec<ArchiveEntry>, bool), WorkspaceServiceError> {
    let file = File::open(path).map_err(WorkspaceServiceError::io)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| invalid_archive())?;
    let mut entries = Vec::new();
    let deadline = Instant::now() + ARCHIVE_PARSE_DEADLINE;
    let mut has_more = false;
    for index in 0..archive.len() {
        if Instant::now() > deadline { has_more = true; break; }
        let file = archive.by_index_raw(index).map_err(|_| invalid_archive())?;
        let Some(path) = safe_entry_path(file.name()) else { continue };
        if entries.len() == WORKSPACE_ARCHIVE_ENTRY_LIMIT { has_more = true; break; }
        entries.push(ArchiveEntry {
            path,
            is_directory: file.is_dir(),
            size: file.size(),
            modified_unix_seconds: file.last_modified().and_then(zip_time_to_unix),
        });
    }
    Ok((entries, has_more))
}

fn sevenz_entries(path: &Path) -> Result<(Vec<ArchiveEntry>, bool), WorkspaceServiceError> {
    let archive = sevenz_rust2::Archive::open(path).map_err(|_| invalid_archive())?;
    let mut entries = Vec::new();
    let mut has_more = false;
    let deadline = Instant::now() + ARCHIVE_PARSE_DEADLINE;
    for file in archive.files {
        if Instant::now() > deadline { has_more = true; break; }
        let Some(path) = safe_entry_path(&file.name) else { continue };
        if entries.len() == WORKSPACE_ARCHIVE_ENTRY_LIMIT { has_more = true; break; }
        let modified_unix_seconds = file.has_last_modified_date.then(|| SystemTime::from(file.last_modified_date))
            .and_then(system_time_to_unix);
        entries.push(ArchiveEntry { path, is_directory: file.is_directory, size: file.size, modified_unix_seconds });
    }
    Ok((entries, has_more))
}

fn tar_entries<R: Read>(reader: R) -> Result<(Vec<ArchiveEntry>, bool), WorkspaceServiceError> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();
    let mut has_more = false;
    let deadline = Instant::now() + ARCHIVE_PARSE_DEADLINE;
    let stream = archive.entries().map_err(|_| invalid_archive())?;
    for result in stream {
        if Instant::now() > deadline { has_more = true; break; }
        let entry = match result {
            Ok(entry) => entry,
            Err(_) if !entries.is_empty() => { has_more = true; break; }
            Err(_) => return Err(invalid_archive()),
        };
        let path = entry.path().map_err(|_| invalid_archive())?;
        let Some(path) = safe_entry_path(&path.to_string_lossy()) else { continue };
        if entries.len() == WORKSPACE_ARCHIVE_ENTRY_LIMIT { has_more = true; break; }
        let header = entry.header();
        entries.push(ArchiveEntry {
            path,
            is_directory: header.entry_type().is_dir(),
            size: header.size().unwrap_or(0),
            modified_unix_seconds: header.mtime().ok(),
        });
    }
    Ok((entries, has_more))
}

fn gzip_entry(path: &Path) -> Result<ArchiveEntry, WorkspaceServiceError> {
    let file = File::open(path).map_err(WorkspaceServiceError::io)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let header = decoder.header().ok_or_else(invalid_archive)?;
    let fallback = path.file_stem().and_then(|value| value.to_str()).unwrap_or("compressed-file");
    let name = header.filename()
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(safe_entry_path)
        .unwrap_or_else(|| fallback.to_string());
    let modified_unix_seconds = (header.mtime() != 0).then_some(u64::from(header.mtime()));
    Ok(ArchiveEntry {
        path: name,
        is_directory: false,
        size: gzip_uncompressed_size(path)?,
        modified_unix_seconds,
    })
}

fn gzip_uncompressed_size(path: &Path) -> Result<u64, WorkspaceServiceError> {
    let mut file = File::open(path).map_err(WorkspaceServiceError::io)?;
    if fs::metadata(path).map_err(WorkspaceServiceError::io)?.len() < 4 { return Err(invalid_archive()); }
    file.seek(SeekFrom::End(-4)).map_err(WorkspaceServiceError::io)?;
    let mut trailer = [0u8; 4];
    file.read_exact(&mut trailer).map_err(WorkspaceServiceError::io)?;
    Ok(u64::from(u32::from_le_bytes(trailer)))
}

fn safe_entry_path(value: &str) -> Option<String> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    let windows_absolute = normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    if normalized.is_empty() || windows_absolute || path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return None;
    }
    Some(normalized.trim_start_matches("./").to_string())
}

fn system_time_to_unix(value: SystemTime) -> Option<u64> {
    value.duration_since(UNIX_EPOCH).ok().map(|duration| duration.as_secs())
}

fn zip_time_to_unix(value: zip::DateTime) -> Option<u64> {
    let year = i64::from(value.year());
    let month = u32::from(value.month());
    let day = u32::from(value.day());
    let days = days_from_civil(year, month, day)?;
    let seconds = days.checked_mul(86_400)?
        .checked_add(i64::from(value.hour()) * 3_600)?
        .checked_add(i64::from(value.minute()) * 60)?
        .checked_add(i64::from(value.second()))?;
    u64::try_from(seconds).ok()
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) { return None; }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn invalid_archive() -> WorkspaceServiceError {
    WorkspaceServiceError::invalid("archive could not be read")
}
