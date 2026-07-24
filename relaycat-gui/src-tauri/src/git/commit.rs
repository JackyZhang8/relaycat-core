use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use super::runner::{git_output, project_root};

pub(super) fn create_commit(project: &str, message: &str, amend: bool) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("commit message is required".into());
    }
    if message.len() > 1024 * 1024 {
        return Err("commit message is too large".into());
    }
    let root = project_root(project)?;
    let repo_root_output = git_output(&root, &["rev-parse", "--show-toplevel"])?;
    if !repo_root_output.status.success() {
        return Err("current project is not a Git repository".into());
    }
    let repo_root = std::path::PathBuf::from(
        String::from_utf8_lossy(&repo_root_output.stdout)
            .trim()
            .to_string(),
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let project_prefix = root.strip_prefix(&repo_root).map_err(|e| e.to_string())?;
    if !project_prefix.as_os_str().is_empty() {
        let staged = git_output(&repo_root, &["diff", "--cached", "--name-only", "-z"])?;
        if !staged.status.success() {
            return Err(String::from_utf8_lossy(&staged.stderr).trim().to_string());
        }
        let outside = staged
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| std::path::PathBuf::from(String::from_utf8_lossy(path).into_owned()))
            .any(|path| !path.starts_with(project_prefix));
        if outside {
            return Err("staged files exist outside the session project".into());
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let message_path = std::env::temp_dir().join(format!(
        "relaycat-commit-{}-{nonce}.txt",
        std::process::id()
    ));
    fs::write(&message_path, message).map_err(|e| e.to_string())?;
    let message_arg = message_path.to_string_lossy().into_owned();
    let mut args = vec!["commit", "-F", message_arg.as_str()];
    if amend {
        args.push("--amend");
    }
    let output = git_output(&root, &args);
    let _ = fs::remove_file(&message_path);
    let output = output?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_repo() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "relaycat-git-commit-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "RelayCat Test"])
            .current_dir(&path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "relaycat@example.test"])
            .current_dir(&path)
            .status()
            .unwrap();
        path
    }

    #[test]
    fn commits_staged_changes_with_a_multiline_message() {
        let repo = temp_repo();
        fs::write(repo.join("file.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(&repo)
            .status()
            .unwrap();
        create_commit(repo.to_str().unwrap(), "标题\n\n说明", false).unwrap();
        let subject = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(&repo)
            .output()
            .unwrap();
        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(String::from_utf8_lossy(&subject.stdout).trim(), "标题");
    }

    #[test]
    fn refuses_to_commit_staged_files_outside_the_session_project() {
        let repo = temp_repo();
        let project = repo.join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("inside.txt"), "inside").unwrap();
        fs::write(repo.join("outside.txt"), "outside").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();

        let result = create_commit(project.to_str().unwrap(), "scoped", false);
        fs::remove_dir_all(&repo).unwrap();
        assert!(result.unwrap_err().contains("outside the session project"));
    }
}
