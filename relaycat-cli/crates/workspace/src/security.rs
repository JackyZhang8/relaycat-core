use relaycat_protocol::{WorkspaceError, WorkspaceErrorCode};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct WorkspaceServiceError {
    error: WorkspaceError,
}

impl WorkspaceServiceError {
    pub fn new(code: WorkspaceErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self { error: WorkspaceError { code, message: message.into(), retryable } }
    }

    pub fn code(&self) -> WorkspaceErrorCode { self.error.code }
    pub fn outside_project() -> Self {
        Self::new(WorkspaceErrorCode::PathOutsideProject, "path is outside the current project", false)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(WorkspaceErrorCode::InvalidRequest, message, false)
    }
    pub fn busy() -> Self { Self::new(WorkspaceErrorCode::Busy, "workspace is busy", true) }
    pub fn timeout() -> Self { Self::new(WorkspaceErrorCode::Timeout, "workspace request timed out", true) }
    pub fn cancelled() -> Self { Self::new(WorkspaceErrorCode::Cancelled, "workspace request was cancelled", true) }
    pub fn unsupported(message: impl Into<String>) -> Self { Self::new(WorkspaceErrorCode::Unsupported, message, false) }

    pub fn io(error: io::Error) -> Self {
        let (code, message, retryable) = match error.kind() {
            io::ErrorKind::NotFound => (WorkspaceErrorCode::NotFound, "path was not found", false),
            io::ErrorKind::PermissionDenied => (WorkspaceErrorCode::PermissionDenied, "permission denied", false),
            _ => (WorkspaceErrorCode::Internal, "workspace I/O failed", true),
        };
        Self::new(code, message, retryable)
    }
}

impl fmt::Display for WorkspaceServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.error.message) }
}
impl std::error::Error for WorkspaceServiceError {}
impl From<WorkspaceServiceError> for WorkspaceError {
    fn from(value: WorkspaceServiceError) -> Self { value.error }
}

#[derive(Debug, Clone)]
pub struct ProjectRoot {
    canonical: PathBuf,
    project_id: String,
}

impl ProjectRoot {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorkspaceServiceError> {
        let canonical = path.as_ref().canonicalize().map_err(WorkspaceServiceError::io)?;
        if !canonical.is_dir() { return Err(WorkspaceServiceError::invalid("project root is not a directory")); }
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let project_id = digest[..16].iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { canonical, project_id })
    }

    pub fn path(&self) -> &Path { &self.canonical }
    pub fn project_id(&self) -> &str { &self.project_id }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf, WorkspaceServiceError> {
        let lexical = self.lexical_path(relative)?;
        let resolved = lexical.canonicalize().map_err(WorkspaceServiceError::io)?;
        if !resolved.starts_with(&self.canonical) { return Err(WorkspaceServiceError::outside_project()); }
        Ok(resolved)
    }

    pub fn lexical_path(&self, relative: &str) -> Result<PathBuf, WorkspaceServiceError> {
        if relative.contains('\0') { return Err(WorkspaceServiceError::outside_project()); }
        let path = Path::new(relative);
        if path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
            return Err(WorkspaceServiceError::outside_project());
        }
        Ok(self.canonical.join(path))
    }
}
