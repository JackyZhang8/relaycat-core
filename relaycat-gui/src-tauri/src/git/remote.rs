use std::time::Duration;

use super::runner::{git_output_with_timeout, project_root};

fn remote_args(operation: &str, force_with_lease: bool) -> Result<Vec<&'static str>, String> {
    match (operation, force_with_lease) {
        ("fetch", false) => Ok(vec!["fetch", "--progress", "--prune"]),
        ("pull", false) => Ok(vec!["pull", "--progress", "--ff-only"]),
        ("push", false) => Ok(vec!["push", "--progress"]),
        ("push", true) => Ok(vec!["push", "--progress", "--force-with-lease"]),
        _ => Err("unsupported remote operation".into()),
    }
}

pub(super) fn run_remote_operation(
    project: &str,
    operation: &str,
    force_with_lease: bool,
) -> Result<String, String> {
    let root = project_root(project)?;
    let args = remote_args(operation, force_with_lease)?;
    let output = git_output_with_timeout(&root, &args, Duration::from_secs(15 * 60))?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!("{stdout}{stderr}").trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_remote_operations() {
        assert!(remote_args("force-push", false).is_err());
        assert_eq!(
            remote_args("push", true).unwrap(),
            vec!["push", "--progress", "--force-with-lease"]
        );
    }

    #[test]
    fn requests_detailed_progress_from_git() {
        assert_eq!(
            remote_args("fetch", false).unwrap(),
            vec!["fetch", "--progress", "--prune"]
        );
        assert_eq!(
            remote_args("pull", false).unwrap(),
            vec!["pull", "--progress", "--ff-only"]
        );
        assert_eq!(
            remote_args("push", false).unwrap(),
            vec!["push", "--progress"]
        );
    }
}
