use super::runner::{git_output, project_root};

fn valid_commit_id(commit: &str) -> bool {
    (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn reset_args(commit: &str, mode: &str) -> Result<Vec<String>, String> {
    if !valid_commit_id(commit) {
        return Err("invalid commit id".into());
    }
    let flag = match mode {
        "soft" => "--soft",
        "mixed" => "--mixed",
        "hard" => "--hard",
        _ => return Err("invalid reset mode".into()),
    };
    Ok(vec!["reset".into(), flag.into(), commit.into()])
}

pub(super) fn commit_action(project: &str, action: &str, commit: &str) -> Result<String, String> {
    if !valid_commit_id(commit) {
        return Err("invalid commit id".into());
    }
    let verb = match action {
        "cherry_pick" => "cherry-pick",
        "revert" => "revert",
        _ => return Err("unsupported commit action".into()),
    };
    let root = project_root(project)?;
    let output = git_output(&root, &[verb, commit])?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub(super) fn reset_to(project: &str, commit: &str, mode: &str) -> Result<(), String> {
    let root = project_root(project)?;
    let args = reset_args(commit, mode)?;
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = git_output(&root, &refs)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_accepts_only_known_modes_and_commit_ids() {
        assert_eq!(
            reset_args("0123456", "soft").unwrap(),
            ["reset", "--soft", "0123456"]
        );
        assert!(reset_args("--help", "hard").is_err());
        assert!(reset_args("0123456", "keep").is_err());
    }
}
