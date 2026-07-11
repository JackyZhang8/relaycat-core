use super::*;

/// Load the launcher config, falling back to the default on a parse error
/// (printed before the TUI takes over the screen).
pub(crate) fn load_config(path: Option<&Path>, language: CliLanguage) -> Config {
    let Some(path) = path else {
        return Config::default();
    };
    match Config::load(path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "{} {}: {err:#}",
                language.t("warning: ignoring invalid config", "警告：忽略无效配置"),
                path.display(),
            );
            Config::default()
        }
    }
}

/// Load the recent-session records (most-recent first), or an empty list if the
/// file is missing or unreadable.
pub(crate) fn load_recent_records() -> Vec<RecentRecord> {
    let Ok(path) = recent_store::recent_file_path() else {
        return Vec::new();
    };
    match recent_store::RecentStore::load(&path) {
        Ok(store) => store.records().to_vec(),
        Err(_) => Vec::new(),
    }
}

/// Build the project picker rows: the current directory and favorites (★)
/// first, then each distinct recent project, then a "custom path" entry last.
pub(crate) fn project_choices(
    records: &[RecentRecord],
    config: &Config,
    language: CliLanguage,
) -> Vec<ProjectChoice> {
    let mut choices = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let cwd = std::env::current_dir().ok();

    let cwd_favorite = cwd.as_deref().map(|cwd| config.is_favorite(cwd));
    let label = match (&cwd, cwd_favorite) {
        (Some(cwd), Some(true)) => format!(
            "★ {}  ({})",
            language.t("Current directory", "当前目录"),
            cwd.display()
        ),
        (Some(cwd), _) => format!(
            "{}  ({})",
            language.t("Current directory", "当前目录"),
            cwd.display()
        ),
        (None, _) => language.t("Current directory", "当前目录").to_string(),
    };
    choices.push(ProjectChoice {
        label,
        kind: ProjectChoiceKind::CurrentDir,
    });
    if let Some(cwd) = cwd {
        seen.push(cwd);
    }

    for favorite in &config.favorites {
        if seen.iter().any(|path| path == favorite) {
            continue;
        }
        seen.push(favorite.clone());
        choices.push(ProjectChoice {
            label: format!("★ {}", favorite.display()),
            kind: ProjectChoiceKind::Path(favorite.clone()),
        });
    }

    for record in records {
        if seen.iter().any(|path| path == &record.project) {
            continue;
        }
        seen.push(record.project.clone());
        choices.push(ProjectChoice {
            label: record.project.display().to_string(),
            kind: ProjectChoiceKind::Path(record.project.clone()),
        });
    }

    choices.push(ProjectChoice {
        label: language
            .t("Enter a custom path…", "输入自定义路径…")
            .to_string(),
        kind: ProjectChoiceKind::Custom,
    });
    choices
}

/// The relay URL to pre-fill for `project`: the relay last used for that exact
/// project if known, then the configured default relay, then the most-recently
/// used relay overall.
pub(crate) fn default_relay_for(
    records: &[RecentRecord],
    project: Option<&Path>,
    config: &Config,
) -> Option<String> {
    let target = match project {
        Some(path) => Some(path.to_path_buf()),
        None => std::env::current_dir().ok(),
    };
    let matched = target
        .as_ref()
        .and_then(|target| records.iter().find(|record| record.project == *target));
    if let Some(record) = matched {
        return Some(record.relay.clone());
    }
    if let Some(default) = &config.default_relay {
        return Some(default.clone());
    }
    records.first().map(|record| record.relay.clone())
}

pub(crate) fn project_index_for_launch(choices: &[ProjectChoice], project: Option<&Path>) -> usize {
    match project {
        None => choices
            .iter()
            .position(|choice| matches!(choice.kind, ProjectChoiceKind::CurrentDir))
            .unwrap_or(0),
        Some(project) => choices
            .iter()
            .position(
                |choice| matches!(&choice.kind, ProjectChoiceKind::Path(path) if path == project),
            )
            .or_else(|| {
                choices
                    .iter()
                    .position(|choice| matches!(choice.kind, ProjectChoiceKind::Custom))
            })
            .unwrap_or(0),
    }
}

pub(crate) fn selected_project<'a>(
    choices: &'a [ProjectChoice],
    state: &ListState,
) -> &'a ProjectChoice {
    let index = state
        .selected()
        .unwrap_or(0)
        .min(choices.len().saturating_sub(1));
    &choices[index]
}
