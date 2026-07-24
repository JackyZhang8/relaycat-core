mod branch;
mod change;
mod commit;
mod history;
mod remote;
pub(crate) mod runner;
mod status;

pub use branch::GitRefDto;
pub use status::RepositorySummaryDto;

#[tauri::command]
pub async fn git_list_refs(project: String) -> Result<Vec<GitRefDto>, String> {
    tauri::async_runtime::spawn_blocking(move || branch::list_refs(&project))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_create_branch(
    project: String,
    name: String,
    start_point: Option<String>,
    checkout: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        branch::create_branch(&project, &name, start_point.as_deref(), checkout)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_checkout_ref(
    project: String,
    reference: String,
    track: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || branch::checkout_ref(&project, &reference, track))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_delete_branch(project: String, name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || branch::delete_branch(&project, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_rename_branch(project: String, name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || branch::rename_branch(&project, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_create_tag(project: String, name: String, target: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || branch::create_tag(&project, &name, &target))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_remote_operation(
    project: String,
    operation: String,
    force_with_lease: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        remote::run_remote_operation(&project, &operation, force_with_lease)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_commit_action(
    project: String,
    action: String,
    commit: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || history::commit_action(&project, &action, &commit))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_reset_to(project: String, commit: String, mode: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || history::reset_to(&project, &commit, &mode))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_create_commit(
    project: String,
    message: String,
    amend: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || commit::create_commit(&project, &message, amend))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_stage_paths(project: String, paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || change::stage_paths(&project, &paths))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_unstage_paths(project: String, paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || change::unstage_paths(&project, &paths))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_discard_paths(project: String, paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || change::discard_paths(&project, &paths))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_apply_patch(
    project: String,
    patch: String,
    cached: bool,
    reverse: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        change::apply_patch(&project, &patch, cached, reverse)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_repository_summary(project: String) -> Result<RepositorySummaryDto, String> {
    tauri::async_runtime::spawn_blocking(move || status::repository_summary(&project))
        .await
        .map_err(|e| e.to_string())?
}
