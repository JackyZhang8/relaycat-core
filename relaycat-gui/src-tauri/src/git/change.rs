use std::io::Write;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use super::runner::{git_output, project_root};

fn validate_paths(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("at least one path is required".into());
    }
    for value in paths {
        let path = Path::new(value);
        if value.is_empty()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err("invalid repository path".into());
        }
    }
    Ok(())
}

fn run_paths(project: &str, prefix: &[&str], paths: &[String]) -> Result<(), String> {
    validate_paths(paths)?;
    let root = project_root(project)?;
    let mut args = prefix.to_vec();
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    let output = git_output(&root, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub(super) fn stage_paths(project: &str, paths: &[String]) -> Result<(), String> {
    run_paths(project, &["add"], paths)
}

pub(super) fn unstage_paths(project: &str, paths: &[String]) -> Result<(), String> {
    let root = project_root(project)?;
    let has_head = git_output(&root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success();
    if has_head {
        run_paths(project, &["restore", "--staged"], paths)
    } else {
        run_paths(
            project,
            &["rm", "--cached", "--quiet", "--ignore-unmatch"],
            paths,
        )
    }
}

pub(super) fn discard_paths(project: &str, paths: &[String]) -> Result<(), String> {
    validate_paths(paths)?;
    let root = project_root(project)?;
    for value in paths {
        let tracked = git_output(&root, &["ls-files", "--error-unmatch", "--", value])?
            .status
            .success();
        if tracked {
            run_paths(
                project,
                &["restore", "--worktree"],
                std::slice::from_ref(value),
            )?;
            continue;
        }
        let target = root.join(value);
        if !target.exists() {
            continue;
        }
        let canonical = target.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(&root) || canonical == root {
            return Err("path escapes project root".into());
        }
        if canonical.is_dir() {
            std::fs::remove_dir_all(canonical).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(canonical).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(super) fn apply_patch(
    project: &str,
    patch: &str,
    cached: bool,
    reverse: bool,
) -> Result<(), String> {
    if patch.is_empty() || patch.len() > 2 * 1024 * 1024 {
        return Err("invalid patch size".into());
    }
    let root = project_root(project)?;
    let mut command = Command::new("git");
    command
        .arg("apply")
        .arg("--recount")
        .arg("--whitespace=nowarn");
    if cached {
        command.arg("--cached");
    }
    if reverse {
        command.arg("--reverse");
    }
    let mut child = command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("git apply stdin unavailable")?
        .write_all(patch.as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_repo() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "relaycat-git-change-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()
            .unwrap();
        path
    }

    #[test]
    fn stages_and_unstages_paths_with_spaces() {
        let repo = temp_repo();
        fs::write(repo.join("hello world.txt"), "hello").unwrap();

        stage_paths(repo.to_str().unwrap(), &["hello world.txt".into()]).unwrap();
        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&staged.stdout).trim(),
            "hello world.txt"
        );

        unstage_paths(repo.to_str().unwrap(), &["hello world.txt".into()]).unwrap();
        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&repo)
            .output()
            .unwrap();
        fs::remove_dir_all(&repo).unwrap();
        assert!(staged.stdout.is_empty());
    }
}
