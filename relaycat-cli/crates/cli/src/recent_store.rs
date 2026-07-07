use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::{RelayOptions, SessionKind, TargetCommand};
use crate::config::relaycat_config_dir;
use crate::i18n::CliLanguage;
use crate::pairing_store::current_unix_timestamp;

const RECENT_FILE: &str = "recent.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRecord {
    pub id: String,
    pub kind: SessionKind,
    pub program: String,
    pub args: Vec<String>,
    pub project: PathBuf,
    pub relay: String,
    pub last_used_at: u64,
    pub use_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentStore {
    records: Vec<RecentRecord>,
}

impl RecentStore {
    pub fn new(records: Vec<RecentRecord>) -> Self {
        let mut store = Self { records };
        store.sort();
        store.repair_duplicate_ids();
        store.sort();
        store
    }

    pub fn records(&self) -> &[RecentRecord] {
        &self.records
    }

    pub fn upsert_target(&mut self, target: &TargetCommand, now_unix: u64) -> bool {
        let Some(relay) = &target.relay else {
            return false;
        };
        let Some(project) = &target.cwd else {
            return false;
        };
        if let Some(record) = self.records.iter_mut().find(|record| {
            record.kind == target.session_kind
                && record.project == *project
                && record.relay == relay.url
                && record.program == target.program
                && record.args == target.args
        }) {
            record.last_used_at = now_unix;
            record.use_count = record.use_count.saturating_add(1);
            self.sort();
            return true;
        }

        let id = self.next_recent_id(&target.session_kind, project);
        self.records.push(RecentRecord {
            id,
            kind: target.session_kind.clone(),
            program: target.program.clone(),
            args: target.args.clone(),
            project: project.clone(),
            relay: relay.url.clone(),
            last_used_at: now_unix,
            use_count: 1,
        });
        self.sort();
        true
    }

    pub fn resolve_target(&self, selector: &str) -> Result<TargetCommand> {
        let record = self.resolve_record(selector)?;
        Ok(TargetCommand {
            program: record.program.clone(),
            args: record.args.clone(),
            cwd: Some(record.project.clone()),
            relay: Some(RelayOptions {
                url: record.relay.clone(),
                room_id: None,
            }),
            session_kind: record.kind.clone(),
        })
    }

    pub fn forget(&mut self, selector: &str) -> Result<RecentRecord> {
        let index = self.resolve_index(selector)?;
        Ok(self.records.remove(index))
    }

    /// Remove all records whose project matches `project`. Returns the number
    /// of records removed.
    pub fn forget_project(&mut self, project: &Path) -> usize {
        let before = self.records.len();
        self.records.retain(|record| record.project != project);
        before - self.records.len()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let file: RecentFile = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(file.into_store())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file = RecentFile::from_store(self);
        let text = serde_json::to_string_pretty(&file)
            .with_context(|| format!("failed to encode {}", path.display()))?;
        fs::write(path, format!("{text}\n"))
            .with_context(|| format!("failed to write {}", path.display()))
    }

    fn resolve_record(&self, selector: &str) -> Result<&RecentRecord> {
        let index = self.resolve_index(selector)?;
        Ok(&self.records[index])
    }

    fn resolve_index(&self, selector: &str) -> Result<usize> {
        if let Ok(number) = selector.parse::<usize>() {
            if number == 0 {
                bail!("recent selector starts at 1");
            }
            let index = number - 1;
            if index < self.records.len() {
                return Ok(index);
            }
        }
        self.records
            .iter()
            .position(|record| record.id == selector)
            .with_context(|| format!("recent session not found: {selector}"))
    }

    fn sort(&mut self) {
        self.records.sort_by(|a, b| {
            b.last_used_at
                .cmp(&a.last_used_at)
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    fn repair_duplicate_ids(&mut self) {
        let mut used = HashSet::new();
        for record in &mut self.records {
            if used.insert(record.id.clone()) {
                continue;
            }

            let base = recent_id_base(&record.kind, &record.project);
            let hash = path_hash(&record.project);
            let mut candidate = if used.contains(&base) {
                format!("{base}-{hash:08x}")
            } else {
                base
            };
            let mut suffix = 2;
            while used.contains(&candidate) {
                candidate = format!(
                    "{}-{hash:08x}-{suffix}",
                    recent_id_base(&record.kind, &record.project)
                );
                suffix += 1;
            }
            record.id = candidate.clone();
            used.insert(candidate);
        }
    }

    fn next_recent_id(&self, kind: &SessionKind, project: &Path) -> String {
        let base = recent_id_base(kind, project);
        if !self.records.iter().any(|record| record.id == base) {
            return base;
        }

        let hash = path_hash(project);
        let mut candidate = format!("{base}-{hash:08x}");
        let mut suffix = 2;
        while self.records.iter().any(|record| record.id == candidate) {
            candidate = format!("{base}-{hash:08x}-{suffix}");
            suffix += 1;
        }
        candidate
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RecentFile {
    version: u8,
    records: Vec<RecentRecordFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecentRecordFile {
    id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd: Option<String>,
    project: PathBuf,
    relay: String,
    last_used_at: u64,
    use_count: u64,
}

impl RecentFile {
    fn from_store(store: &RecentStore) -> Self {
        Self {
            version: 1,
            records: store
                .records()
                .iter()
                .map(|record| RecentRecordFile {
                    id: record.id.clone(),
                    kind: record.kind.as_str().to_string(),
                    program: Some(record.program.clone()),
                    args: Some(record.args.clone()),
                    cmd: None,
                    project: record.project.clone(),
                    relay: record.relay.clone(),
                    last_used_at: record.last_used_at,
                    use_count: record.use_count,
                })
                .collect(),
        }
    }

    fn into_store(self) -> RecentStore {
        let records = self
            .records
            .into_iter()
            .filter_map(|record| {
                let kind = SessionKind::new(record.kind).ok()?;
                let program = record
                    .program
                    .or(record.cmd)
                    .unwrap_or_else(|| kind.as_str().to_string());
                Some(RecentRecord {
                    id: record.id,
                    kind,
                    program,
                    args: record.args.unwrap_or_default(),
                    project: record.project,
                    relay: record.relay,
                    last_used_at: record.last_used_at,
                    use_count: record.use_count,
                })
            })
            .collect();
        RecentStore::new(records)
    }
}

/// Record `target` in the on-disk recent-sessions list (load, upsert, save).
/// No-op for targets without a relay or working directory. Shared by the CLI
/// dispatch and the TUI launcher so both keep `recent.json` consistent.
pub fn remember_recent_target(target: &TargetCommand) -> Result<()> {
    let path = recent_file_path()?;
    let mut store = RecentStore::load(&path)?;
    if store.upsert_target(target, current_unix_timestamp()) {
        store.save(&path)?;
    }
    Ok(())
}

pub fn remember_recent_target_or_warn(target: &TargetCommand) {
    if let Err(err) = remember_recent_target(target) {
        let language = CliLanguage::from_system_locale();
        eprintln!(
            "{}: {err:#}",
            language.t(
                "warning: failed to update recent sessions",
                "警告：更新最近会话失败",
            )
        );
    }
}

pub fn recent_file_path() -> Result<PathBuf> {
    Ok(relaycat_config_dir()?.join(RECENT_FILE))
}

pub fn format_recent_list(store: &RecentStore) -> String {
    format_recent_list_for_language(store, CliLanguage::from_system_locale())
}

pub fn format_recent_list_for_language(store: &RecentStore, language: CliLanguage) -> String {
    if store.records().is_empty() {
        return format!(
            "{}\n",
            language.t("No recent relaycat sessions.", "没有最近的 relaycat 会话。")
        );
    }
    let mut output = format!(
        "{}\n\n",
        language.t("Recent relaycat sessions:", "最近的 relaycat 会话：")
    );
    for (index, record) in store.records().iter().enumerate() {
        output.push_str(&format!(
            "  {:>2}  {:<8}  {:<40}  {:<28}  {} {}x\n",
            index + 1,
            record.kind.as_str(),
            record.project.display(),
            record.relay,
            language.t("used", "使用"),
            record.use_count
        ));
    }
    output.push_str(&format!(
        "\n{}:\n  relaycat run 1\n",
        language.t("Run", "运行")
    ));
    output
}

fn recent_id_base(kind: &SessionKind, project: &Path) -> String {
    let name = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let name = slug(name);
    let name = if name.is_empty() { "project" } else { &name };
    format!("{}-{name}", kind.as_str())
}

fn slug(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn path_hash(path: &Path) -> u32 {
    const FNV_OFFSET: u32 = 0x811c9dc5;
    const FNV_PRIME: u32 = 0x01000193;

    path.to_string_lossy()
        .bytes()
        .fold(FNV_OFFSET, |hash, byte| {
            hash.wrapping_mul(FNV_PRIME) ^ u32::from(byte)
        })
}
