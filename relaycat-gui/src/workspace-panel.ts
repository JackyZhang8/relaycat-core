import { invoke } from "@tauri-apps/api/core";

import {
  groupGitChanges,
  gitHistoryPage,
  gitStatusFingerprint,
  historyNearBottom,
  previewLanguage,
  projectBasename,
  storedWorkspacePanelWidth,
  tokenizePreviewLine,
  workspacePanelWidth,
  workspaceEntryDecoration,
  type FilePreview,
  type GitCommit,
  type GitDisplayChange,
  type GitStatus,
  type WorkspaceEntry,
} from "./workspace-model";

type WorkspaceMode = "files" | "git" | "history";

export interface WorkspacePanelController {
  toggle(): void;
  close(): void;
  setProject(project: string | null): void;
  refresh(): Promise<void>;
}

interface WorkspacePanelOptions {
  translate: (key: string, ...args: (string | number)[]) => string;
  afterLayoutChange: () => void;
}

const WIDTH_STORAGE_KEY = "relaycat.workspacePanelWidth";

export function createWorkspacePanel(
  options: WorkspacePanelOptions,
): WorkspacePanelController {
  const { translate: t, afterLayoutChange } = options;
  const stage = document.querySelector(".stage") as HTMLElement;
  const panel = document.querySelector("#workspace-panel") as HTMLElement;
  const resizer = document.querySelector("#workspace-resizer") as HTMLElement;
  const toggleBtn = document.querySelector("#btn-workspace") as HTMLButtonElement;
  const closeBtn = document.querySelector("#workspace-close") as HTMLButtonElement;
  const refreshBtn = document.querySelector("#workspace-refresh") as HTMLButtonElement;
  const projectLabel = document.querySelector("#workspace-project") as HTMLElement;
  const filesTab = document.querySelector("#workspace-tab-files") as HTMLButtonElement;
  const gitTab = document.querySelector("#workspace-tab-git") as HTMLButtonElement;
  const historyTab = document.querySelector("#workspace-tab-history") as HTMLButtonElement;
  const filesView = document.querySelector("#workspace-files-view") as HTMLElement;
  const gitView = document.querySelector("#workspace-git-view") as HTMLElement;
  const historyView = document.querySelector("#workspace-history-view") as HTMLElement;
  const fileTree = document.querySelector("#workspace-file-tree") as HTMLElement;
  const gitMeta = document.querySelector("#workspace-git-meta") as HTMLElement;
  const gitList = document.querySelector("#workspace-git-list") as HTMLElement;
  const historyList = document.querySelector("#workspace-history-list") as HTMLElement;
  const previewTitle = document.querySelector("#workspace-preview-title") as HTMLElement;
  const previewNotice = document.querySelector("#workspace-preview-notice") as HTMLElement;
  const previewImage = document.querySelector("#workspace-preview-image") as HTMLImageElement;
  const previewCode = document.querySelector("#workspace-preview-code") as HTMLElement;

  let project: string | null = null;
  let mode: WorkspaceMode = "files";
  let opened = false;
  let projectRevision = 0;
  let refreshCount = 0;
  let refreshTimer: number | null = null;
  let resizing = false;
  let layoutFrame: number | null = null;
  let lastGitFingerprint: string | null = null;
  let historyCommits: GitCommit[] = [];
  let historyLoading = false;
  let historyHasMore = true;

  panel.style.width = `${storedWorkspacePanelWidth(localStorage.getItem(WIDTH_STORAGE_KEY))}px`;

  function scheduleLayoutChange() {
    if (layoutFrame !== null) cancelAnimationFrame(layoutFrame);
    layoutFrame = requestAnimationFrame(() => {
      layoutFrame = null;
      afterLayoutChange();
    });
  }

  function renderState(container: HTMLElement, message: string) {
    container.innerHTML = "";
    const state = document.createElement("div");
    state.className = "ws-state";
    state.textContent = message;
    container.appendChild(state);
  }

  function clearPreview() {
    previewTitle.textContent = t("workspace_preview");
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    previewImage.hidden = true;
    previewImage.removeAttribute("src");
    previewCode.hidden = false;
    previewCode.textContent = "";
  }

  function formatBytes(size: number): string {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
    return `${(size / (1024 * 1024)).toFixed(1)} MiB`;
  }

  function renderHighlightedText(content: string, path: string) {
    const language = previewLanguage(path);
    previewCode.innerHTML = "";
    if (!language) {
      previewCode.textContent = content;
      return;
    }
    for (const line of content.split("\n")) {
      const lineEl = document.createElement("span");
      lineEl.className = "ws-code-line";
      for (const token of tokenizePreviewLine(line, language)) {
        const tokenEl = document.createElement("span");
        tokenEl.className = `ws-token ${token.kind}`;
        tokenEl.textContent = token.text;
        lineEl.appendChild(tokenEl);
      }
      previewCode.appendChild(lineEl);
    }
  }

  function renderPreview(
    title: string,
    preview: FilePreview,
    diff: boolean,
    sourcePath = title,
  ) {
    previewTitle.textContent = title;
    previewImage.hidden = true;
    previewImage.removeAttribute("src");
    previewCode.hidden = false;
    previewCode.innerHTML = "";
    let notice = "";
    if (preview.kind === "binary") notice = t("workspace_binary");
    else if (preview.kind === "too_large") {
      notice = t("workspace_too_large", formatBytes(preview.size_bytes));
    }
    previewNotice.textContent = notice;
    previewNotice.classList.toggle("show", !!notice);
    if (notice) return;
    if (preview.kind === "image") {
      previewCode.hidden = true;
      previewImage.src = preview.content;
      previewImage.hidden = false;
      return;
    }
    if (!preview.content) {
      previewCode.textContent = diff ? t("workspace_diff_empty") : "";
      return;
    }
    if (!diff) {
      renderHighlightedText(preview.content, sourcePath);
      return;
    }
    for (const line of preview.content.split("\n")) {
      const lineEl = document.createElement("span");
      lineEl.className = "ws-diff-line";
      if (line.startsWith("+") && !line.startsWith("+++")) lineEl.classList.add("add");
      else if (line.startsWith("-") && !line.startsWith("---")) lineEl.classList.add("del");
      else if (line.startsWith("@@")) lineEl.classList.add("hunk");
      lineEl.textContent = line || " ";
      previewCode.appendChild(lineEl);
    }
  }

  function showPreviewLoading(title: string) {
    previewTitle.textContent = title;
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    previewImage.hidden = true;
    previewImage.removeAttribute("src");
    previewCode.hidden = false;
    previewCode.textContent = t("workspace_loading");
  }

  function renderLoadError(container: HTMLElement, error: unknown) {
    renderState(container, t("workspace_load_failed", String(error)));
  }

  async function previewFile(relativePath: string, title = relativePath) {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    showPreviewLoading(title);
    try {
      const preview = await invoke<FilePreview>("read_workspace_file", {
        project: activeProject,
        relativePath,
        maxBytes: 512 * 1024,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      renderPreview(title, preview, false, relativePath);
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      previewCode.textContent = t("workspace_load_failed", String(error));
    }
  }

  function renderTreeEntries(
    entries: WorkspaceEntry[],
    container: HTMLElement,
    depth: number,
    activeProject: string,
    revision: number,
  ) {
    container.innerHTML = "";
    if (entries.length === 0) {
      renderState(container, t("workspace_empty_dir"));
      return;
    }
    for (const entry of entries) {
      const node = document.createElement("div");
      node.className = "ws-tree-node";
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-tree-row";
      row.style.paddingLeft = `${8 + depth * 14}px`;
      row.title = entry.relative_path;
      const arrow = document.createElement("span");
      arrow.className = "ws-tree-arrow";
      const decoration = workspaceEntryDecoration(entry.is_dir);
      arrow.textContent = decoration.disclosure;
      const icon = document.createElement("span");
      icon.className = "ws-tree-icon";
      icon.textContent = decoration.icon;
      const name = document.createElement("span");
      name.className = "ws-tree-name";
      name.textContent = entry.name;
      row.append(arrow);
      if (decoration.icon) row.append(icon);
      row.append(name);
      node.appendChild(row);

      if (entry.is_dir) {
        const children = document.createElement("div");
        children.className = "ws-tree-children";
        children.hidden = true;
        node.appendChild(children);
        let loaded = false;
        row.onclick = async () => {
          if (!children.hidden) {
            children.hidden = true;
            arrow.textContent = "›";
            return;
          }
          children.hidden = false;
          arrow.textContent = "⌄";
          if (loaded) return;
          renderState(children, t("workspace_loading"));
          try {
            const childEntries = await invoke<WorkspaceEntry[]>("list_workspace_entries", {
              project: activeProject,
              relativePath: entry.relative_path,
            });
            if (project !== activeProject || projectRevision !== revision) return;
            loaded = true;
            renderTreeEntries(childEntries, children, depth + 1, activeProject, revision);
          } catch (error) {
            if (project !== activeProject || projectRevision !== revision) return;
            renderLoadError(children, error);
          }
        };
      } else {
        row.onclick = () => void previewFile(entry.relative_path);
      }
      container.appendChild(node);
    }
  }

  async function refreshFiles() {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    renderState(fileTree, t("workspace_loading"));
    try {
      const entries = await invoke<WorkspaceEntry[]>("list_workspace_entries", {
        project: activeProject,
        relativePath: "",
      });
      if (project !== activeProject || projectRevision !== revision) return;
      renderTreeEntries(entries, fileTree, 0, activeProject, revision);
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      renderLoadError(fileTree, error);
    }
  }

  async function previewGitChange(change: GitDisplayChange) {
    if (!project) return;
    if (!change.staged && change.status === "?") {
      await previewFile(change.path, `${change.path} · ${t("workspace_untracked_preview")}`);
      return;
    }
    const activeProject = project;
    const revision = projectRevision;
    showPreviewLoading(change.path);
    try {
      const preview = await invoke<FilePreview>("git_workspace_diff", {
        project: activeProject,
        relativePath: change.path,
        staged: change.staged,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      renderPreview(change.path, preview, true);
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      previewCode.textContent = t("workspace_load_failed", String(error));
    }
  }

  function renderGitSection(title: string, changes: GitDisplayChange[]) {
    if (changes.length === 0) return;
    const heading = document.createElement("div");
    heading.className = "ws-git-section-title";
    heading.textContent = `${title} (${changes.length})`;
    gitList.appendChild(heading);
    for (const change of changes) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-git-row";
      row.title = change.path;
      const status = document.createElement("span");
      status.className = "ws-git-status";
      status.textContent = change.status;
      const path = document.createElement("span");
      path.className = "ws-git-path";
      path.textContent = change.path;
      row.append(status, path);
      row.onclick = () => void previewGitChange(change);
      gitList.appendChild(row);
    }
  }

  async function refreshGit(background = false) {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    if (!background && lastGitFingerprint === null) {
      renderState(gitList, t("workspace_loading"));
      gitMeta.textContent = "";
    }
    try {
      const status = await invoke<GitStatus>("git_workspace_status", {
        project: activeProject,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      const fingerprint = gitStatusFingerprint(status);
      if (fingerprint === lastGitFingerprint) return;
      lastGitFingerprint = fingerprint;
      if (!status.is_repo) {
        gitMeta.textContent = "";
        renderState(gitList, t("workspace_not_repo"));
        return;
      }
      gitMeta.textContent = status.branch ? `⎇ ${status.branch}` : "Git";
      const sections = groupGitChanges(status.changes);
      gitList.innerHTML = "";
      if (sections.staged.length === 0 && sections.unstaged.length === 0) {
        renderState(gitList, t("workspace_no_changes"));
        return;
      }
      renderGitSection(t("workspace_staged"), sections.staged);
      renderGitSection(t("workspace_unstaged"), sections.unstaged);
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      if (background) {
        console.error("workspace git refresh failed", error);
        return;
      }
      renderLoadError(gitList, error);
    }
  }

  async function previewCommit(commit: GitCommit) {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    const title = t("workspace_commit_diff", commit.short_hash);
    showPreviewLoading(title);
    try {
      const preview = await invoke<FilePreview>("git_workspace_commit_diff", {
        project: activeProject,
        commit: commit.hash,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      renderPreview(title, preview, true);
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      previewCode.textContent = t("workspace_load_failed", String(error));
    }
  }

  function appendHistoryRows(commits: GitCommit[]) {
    for (const commit of commits) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-history-row";
      row.title = commit.subject;
      const hash = document.createElement("span");
      hash.className = "ws-history-hash";
      hash.textContent = commit.short_hash;
      const body = document.createElement("span");
      body.className = "ws-history-body";
      const subject = document.createElement("span");
      subject.className = "ws-history-subject";
      subject.textContent = commit.subject;
      const meta = document.createElement("span");
      meta.className = "ws-history-meta";
      const parsedDate = new Date(commit.date);
      const date = Number.isNaN(parsedDate.getTime())
        ? commit.date
        : parsedDate.toLocaleString();
      meta.textContent = `${commit.author} · ${date}`;
      body.append(subject, meta);
      row.append(hash, body);
      row.onclick = () => void previewCommit(commit);
      historyList.appendChild(row);
    }
  }

  async function refreshHistory(reset = true) {
    if (!project || historyLoading || (!reset && !historyHasMore)) return;
    const activeProject = project;
    const revision = projectRevision;
    historyLoading = true;
    if (reset) {
      historyCommits = [];
      historyHasMore = true;
      historyView.scrollTop = 0;
      renderState(historyList, t("workspace_loading"));
    }
    try {
      if (reset) {
        const status = await invoke<GitStatus>("git_workspace_status", {
          project: activeProject,
        });
        if (project !== activeProject || projectRevision !== revision) return;
        if (!status.is_repo) {
          historyHasMore = false;
          renderState(historyList, t("workspace_not_repo"));
          return;
        }
      }
      const page = gitHistoryPage(historyCommits.length);
      const commits = await invoke<GitCommit[]>("git_workspace_history", {
        project: activeProject,
        skip: page.skip,
        limit: page.limit,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      if (reset) historyList.innerHTML = "";
      historyCommits.push(...commits);
      historyHasMore = commits.length === page.limit;
      if (historyCommits.length === 0) {
        renderState(historyList, t("workspace_history_empty"));
      } else {
        appendHistoryRows(commits);
      }
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      renderLoadError(historyList, error);
    } finally {
      historyLoading = false;
    }
  }

  async function refresh(background = false) {
    if (!opened || !project) return;
    if (!background) {
      refreshCount += 1;
      refreshBtn.disabled = true;
    }
    try {
      if (mode === "files") await refreshFiles();
      else if (mode === "git") await refreshGit(background);
      else await refreshHistory(true);
    } finally {
      if (!background) {
        refreshCount -= 1;
        refreshBtn.disabled = refreshCount > 0;
      }
    }
  }

  function startRefreshTimer() {
    if (refreshTimer !== null) window.clearInterval(refreshTimer);
    refreshTimer = window.setInterval(() => {
      if (opened && mode === "git") void refresh(true);
    }, 3000);
  }

  function stopRefreshTimer() {
    if (refreshTimer === null) return;
    window.clearInterval(refreshTimer);
    refreshTimer = null;
  }

  function setOpened(next: boolean) {
    opened = next && !!project;
    panel.classList.toggle("open", opened);
    stage.classList.toggle("workspace-open", opened);
    toggleBtn.classList.toggle("active", opened);
    panel.setAttribute("aria-hidden", String(!opened));
    resizer.setAttribute("aria-hidden", String(!opened));
    if (opened) {
      startRefreshTimer();
      void refresh();
    } else {
      stopRefreshTimer();
    }
    scheduleLayoutChange();
  }

  function setMode(next: WorkspaceMode) {
    mode = next;
    const filesActive = mode === "files";
    const gitActive = mode === "git";
    const historyActive = mode === "history";
    filesTab.classList.toggle("active", filesActive);
    filesTab.setAttribute("aria-selected", String(filesActive));
    gitTab.classList.toggle("active", gitActive);
    gitTab.setAttribute("aria-selected", String(gitActive));
    historyTab.classList.toggle("active", historyActive);
    historyTab.setAttribute("aria-selected", String(historyActive));
    filesView.hidden = !filesActive;
    gitView.hidden = !gitActive;
    historyView.hidden = !historyActive;
    clearPreview();
    void refresh();
  }

  toggleBtn.onclick = () => setOpened(!opened);
  closeBtn.onclick = () => setOpened(false);
  refreshBtn.onclick = () => void refresh();
  filesTab.onclick = () => setMode("files");
  gitTab.onclick = () => setMode("git");
  historyTab.onclick = () => setMode("history");
  historyView.addEventListener(
    "scroll",
    () => {
      if (
        mode === "history" &&
        historyNearBottom(
          historyView.scrollTop,
          historyView.clientHeight,
          historyView.scrollHeight,
        )
      ) {
        void refreshHistory(false);
      }
    },
    { passive: true },
  );

  resizer.addEventListener("pointerdown", (event) => {
    if (window.matchMedia("(max-width: 760px)").matches) return;
    event.preventDefault();
    resizing = true;
    document.body.classList.add("ws-resizing");
    resizer.setPointerCapture(event.pointerId);
  });
  resizer.addEventListener("pointermove", (event) => {
    if (!resizing) return;
    const width = workspacePanelWidth(window.innerWidth - event.clientX);
    panel.style.width = `${width}px`;
    scheduleLayoutChange();
  });
  const finishResize = (event: PointerEvent) => {
    if (!resizing) return;
    resizing = false;
    document.body.classList.remove("ws-resizing");
    if (resizer.hasPointerCapture(event.pointerId)) resizer.releasePointerCapture(event.pointerId);
    localStorage.setItem(WIDTH_STORAGE_KEY, String(panel.getBoundingClientRect().width));
    scheduleLayoutChange();
  };
  resizer.addEventListener("pointerup", finishResize);
  resizer.addEventListener("pointercancel", finishResize);

  clearPreview();
  renderState(fileTree, t("workspace_loading"));
  renderState(gitList, t("workspace_loading"));
  renderState(historyList, t("workspace_loading"));

  return {
    toggle: () => setOpened(!opened),
    close: () => setOpened(false),
    setProject(nextProject) {
      if (project === nextProject) return;
      project = nextProject;
      projectRevision += 1;
      lastGitFingerprint = null;
      historyCommits = [];
      historyLoading = false;
      historyHasMore = true;
      projectLabel.textContent = project ? projectBasename(project) : "";
      projectLabel.title = project ?? "";
      clearPreview();
      renderState(fileTree, project ? t("workspace_loading") : "");
      renderState(gitList, project ? t("workspace_loading") : "");
      renderState(historyList, project ? t("workspace_loading") : "");
      gitMeta.textContent = "";
      if (!project) setOpened(false);
      else if (opened) void refresh();
    },
    refresh: () => refresh(false),
  };
}
