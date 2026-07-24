use serde::Serialize;

use super::runner::{git_output, git_text, project_root};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitRefDto {
    pub name: String,
    pub kind: String,
    pub current: bool,
}

fn validate_branch(root: &std::path::Path, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("branch name is required".into());
    }
    let output = git_output(root, &["check-ref-format", "--branch", name])?;
    if output.status.success() {
        Ok(())
    } else {
        Err("invalid branch name".into())
    }
}

pub(super) fn list_refs(project: &str) -> Result<Vec<GitRefDto>, String> {
    let root = project_root(project)?;
    let current = git_text(&root, &["branch", "--show-current"])?;
    let current = current.trim();
    let mut refs = Vec::new();
    for (prefix, kind) in [
        ("refs/heads", "local"),
        ("refs/remotes", "remote"),
        ("refs/tags", "tag"),
    ] {
        let output = git_text(
            &root,
            &["for-each-ref", "--format=%(refname:short)", prefix],
        )?;
        refs.extend(
            output
                .lines()
                .filter(|line| !line.is_empty())
                .map(|name| GitRefDto {
                    name: name.to_string(),
                    kind: kind.to_string(),
                    current: kind == "local" && name == current,
                }),
        );
    }
    Ok(refs)
}

pub(super) fn create_branch(
    project: &str,
    name: &str,
    start_point: Option<&str>,
    checkout: bool,
) -> Result<(), String> {
    let root = project_root(project)?;
    validate_branch(&root, name)?;
    let mut args = vec![if checkout { "switch" } else { "branch" }];
    if checkout {
        args.extend(["-c", name]);
    } else {
        args.push(name);
    }
    if let Some(start) = start_point.map(str::trim).filter(|value| !value.is_empty()) {
        if start.starts_with('-') {
            return Err("invalid branch start point".into());
        }
        let commit = format!("{start}^{{commit}}");
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
            return Err("invalid branch start point".into());
        }
        args.push(start);
    }
    let output = git_output(&root, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn checkout_args(reference: &str, track: bool) -> Result<Vec<&str>, String> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err("invalid ref".into());
    }
    let mut args = vec!["switch"];
    if track {
        args.push("--track");
    }
    args.push(reference);
    Ok(args)
}

pub(super) fn checkout_ref(project: &str, reference: &str, track: bool) -> Result<(), String> {
    let root = project_root(project)?;
    let args = checkout_args(reference, track)?;
    let output = git_output(&root, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub(super) fn delete_branch(project: &str, name: &str) -> Result<(), String> {
    let root = project_root(project)?;
    validate_branch(&root, name)?;
    let output = git_output(&root, &["branch", "-d", name])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub(super) fn rename_branch(project: &str, name: &str) -> Result<(), String> {
    let root = project_root(project)?;
    validate_branch(&root, name)?;
    let output = git_output(&root, &["branch", "-m", name])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub(super) fn create_tag(project: &str, name: &str, target: &str) -> Result<(), String> {
    if name.is_empty() || target.is_empty() || name.starts_with('-') || target.starts_with('-') {
        return Err("invalid tag".into());
    }
    let root = project_root(project)?;
    let full_ref = format!("refs/tags/{name}");
    if !git_output(&root, &["check-ref-format", full_ref.as_str()])?
        .status
        .success()
    {
        return Err("invalid tag name".into());
    }
    let output = git_output(&root, &["tag", name, target])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_REPO_SEQ: AtomicU64 = AtomicU64::new(1);

    fn temp_repo() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "relaycat-git-branch-{}-{nonce}-{}",
            std::process::id(),
            TEST_REPO_SEQ.fetch_add(1, Ordering::Relaxed)
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
        fs::write(path.join("file.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "--quiet", "-m", "initial"])
            .current_dir(&path)
            .status()
            .unwrap();
        path
    }

    #[test]
    fn creates_and_checks_out_a_branch() {
        let repo = temp_repo();
        create_branch(repo.to_str().unwrap(), "feature/test", None, true).unwrap();
        rename_branch(repo.to_str().unwrap(), "feature/renamed").unwrap();
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&repo)
            .output()
            .unwrap();
        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&branch.stdout).trim(),
            "feature/renamed"
        );
    }

    #[test]
    fn remote_checkout_uses_tracking_mode() {
        assert_eq!(
            checkout_args("origin/topic", true).unwrap(),
            vec!["switch", "--track", "origin/topic"]
        );
    }

    #[test]
    fn rejects_option_like_branch_start_points() {
        let repo = temp_repo();
        let result = create_branch(
            repo.to_str().unwrap(),
            "feature/safe",
            Some("--discard-changes"),
            true,
        );
        fs::remove_dir_all(&repo).unwrap();
        assert!(result.unwrap_err().contains("invalid branch start point"));
    }
}
