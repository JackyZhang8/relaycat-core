use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_GIT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_NOT_INSTALLED_ERROR: &str = "git_not_installed";

pub fn project_root(project: &str) -> Result<PathBuf, String> {
    let root = Path::new(project)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("project path is not a directory".into());
    }
    Ok(root)
}

pub fn git_output(project: &Path, args: &[&str]) -> Result<Output, String> {
    git_output_with_timeout(project, args, DEFAULT_GIT_TIMEOUT)
}

pub fn git_output_with_timeout(
    project: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(project)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(git_spawn_error)?;
    let stdout = child.stdout.take().ok_or("git stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("git stderr unavailable")?;
    let stdout_reader = thread::spawn(move || read_bounded_stream(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_stream(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err("git command timed out".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "git stdout reader failed")??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "git stderr reader failed")??;
    if stdout_truncated || stderr_truncated {
        return Err("git output exceeded limit".into());
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn git_spawn_error(error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        GIT_NOT_INSTALLED_ERROR.to_string()
    } else {
        error.to_string()
    }
}

fn read_bounded_stream(mut stream: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        let remaining = MAX_GIT_OUTPUT_BYTES.saturating_sub(output.len());
        let kept = remaining.min(count);
        output.extend_from_slice(&buffer[..kept]);
        truncated |= kept < count;
    }
    Ok((output, truncated))
}

pub fn git_text(project: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(project, args)?;
    if !output.status.success() {
        return Err(bounded_text(&output.stderr, MAX_GIT_OUTPUT_BYTES));
    }
    Ok(bounded_text(&output.stdout, MAX_GIT_OUTPUT_BYTES))
}

fn bounded_text(bytes: &[u8], max_bytes: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut used = 0;
    text.chars()
        .take_while(|ch| {
            let next = used + ch.len_utf8();
            if next > max_bytes {
                return false;
            }
            used = next;
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn missing_git_executable_has_a_stable_error_code() {
        assert_eq!(
            git_spawn_error(io::Error::from(io::ErrorKind::NotFound)),
            "git_not_installed"
        );
    }

    #[test]
    fn output_is_capped_without_splitting_utf8() {
        assert_eq!(bounded_text("ab中文cd".as_bytes(), 5), "ab中");
    }

    #[test]
    fn command_output_is_rejected_after_the_limit() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repo = std::env::temp_dir().join(format!(
            "relaycat-git-output-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&repo).unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let large = repo.join("large.bin");
        fs::write(&large, vec![b'x'; MAX_GIT_OUTPUT_BYTES + 1]).unwrap();
        let hash = Command::new("git")
            .args(["hash-object", "-w", "large.bin"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let hash = String::from_utf8_lossy(&hash.stdout).trim().to_string();

        let result = git_output(&repo, &["cat-file", "blob", &hash]);
        fs::remove_dir_all(&repo).unwrap();
        assert!(result.unwrap_err().contains("output exceeded"));
    }
}
