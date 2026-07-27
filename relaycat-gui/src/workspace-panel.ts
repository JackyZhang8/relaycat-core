import { invoke } from "@tauri-apps/api/core";

import { renderSafeMarkdown } from "./markdown-preview";
import {
  developerPreviewKind,
  parseDelimitedPreview,
  sanitizeSvgPreview,
  type DeveloperPreviewKind,
} from "./developer-preview";

import {
  canCommit,
  buildSelectedLinesPatch,
  gitOperationControls,
  gitMutationUsesInlineFeedback,
  gitSyncButtonLabels,
  historyFilterArgs,
  parseUnifiedDiff,
  reconcileGitChangeKeys,
} from "./git-model";

import {
  groupGitChanges,
  gitHistoryPage,
  gitStatusFingerprint,
  historyNearBottom,
  canRenderMarkdown,
  formatWorkspaceEntrySize,
  formatWorkspaceModifiedTime,
  isMarkdownPreviewPath,
  previewLanguage,
  projectBasename,
  storedWorkspacePanelWidth,
  tokenizeDiffLine,
  tokenizePreviewLine,
  workspaceCommitPresentation,
  workspacePanelWidth,
  workspaceLocalPreviewLimit,
  workspaceEntryDecoration,
  workspacePreviewStartsCollapsed,
  type FilePreview,
  type GitCommit,
  type GitDisplayChange,
  type GitStatus,
  type WorkspaceEntry,
  type WorkspaceEntriesPage,
} from "./workspace-model";

type WorkspaceMode = "files" | "git" | "history" | "shell";
export type WorkspaceEntryMode = WorkspaceMode | null;

export interface WorkspacePanelController {
  show(mode: WorkspaceEntryMode): void;
  close(): void;
  setProject(project: string | null): void;
  refresh(): Promise<void>;
}

interface WorkspacePanelOptions {
  translate: (key: string, ...args: (string | number)[]) => string;
  afterLayoutChange: () => void;
  confirm: (message: string) => Promise<boolean>;
  onClose: () => void;
  onModeChange: (mode: Exclude<WorkspaceEntryMode, null>) => void;
}

interface RepositorySummary {
  is_repo: boolean;
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  operation: string | null;
}

interface GitRef {
  name: string;
  kind: "local" | "remote" | "tag";
  current: boolean;
}

const WIDTH_STORAGE_KEYS = {
  files: "relaycat.workspacePanelWidth.files",
  git: "relaycat.workspacePanelWidth.git",
  shell: "relaycat.workspacePanelWidth.shell",
} as const;
const LEGACY_WIDTH_STORAGE_KEY = "relaycat.workspacePanelWidth";
const PREVIEW_HEIGHT_STORAGE_KEY = "relaycat.workspacePreviewHeight";

export function createWorkspacePanel(
  options: WorkspacePanelOptions,
): WorkspacePanelController {
  const { translate: t, afterLayoutChange, confirm, onClose, onModeChange } = options;
  const stage = document.querySelector(".stage") as HTMLElement;
  const panel = document.querySelector("#workspace-panel") as HTMLElement;
  const resizer = document.querySelector("#workspace-resizer") as HTMLElement;
  const closeBtn = document.querySelector("#workspace-close") as HTMLButtonElement;
  const refreshBtn = document.querySelector("#workspace-refresh") as HTMLButtonElement;
  const refreshProgress = document.querySelector("#workspace-refresh-progress") as HTMLElement;
  const projectLabel = document.querySelector("#workspace-project") as HTMLElement;
  const gitTab = document.querySelector("#workspace-tab-git") as HTMLButtonElement;
  const historyTab = document.querySelector("#workspace-tab-history") as HTMLButtonElement;
  const filesView = document.querySelector("#workspace-files-view") as HTMLElement;
  const gitView = document.querySelector("#workspace-git-view") as HTMLElement;
  const historyView = document.querySelector("#workspace-history-view") as HTMLElement;
  const fileTree = document.querySelector("#workspace-file-tree") as HTMLElement;
  const gitMeta = document.querySelector("#workspace-git-meta") as HTMLElement;
  const gitList = document.querySelector("#workspace-git-list") as HTMLElement;
  const operationEl = document.querySelector("#workspace-operation") as HTMLElement;
  const branchSelect = document.querySelector("#workspace-branch") as HTMLSelectElement;
  const fetchBtn = document.querySelector("#workspace-fetch") as HTMLButtonElement;
  const pullBtn = document.querySelector("#workspace-pull") as HTMLButtonElement;
  const pushBtn = document.querySelector("#workspace-push") as HTMLButtonElement;
  const stageAllBtn = document.querySelector("#workspace-stage-all") as HTMLButtonElement;
  const unstageAllBtn = document.querySelector("#workspace-unstage-all") as HTMLButtonElement;
  const commitMessage = document.querySelector("#workspace-commit-message") as HTMLTextAreaElement;
  const commitLauncher = document.querySelector("#workspace-commit-launcher") as HTMLButtonElement;
  const commitBox = document.querySelector("#workspace-commit-box") as HTMLElement;
  const commitClose = document.querySelector("#workspace-commit-close") as HTMLButtonElement;
  const amendInput = document.querySelector("#workspace-amend") as HTMLInputElement;
  const commitBtn = document.querySelector("#workspace-commit") as HTMLButtonElement;
  const commitPushBtn = document.querySelector("#workspace-commit-push") as HTMLButtonElement;
  const historyList = document.querySelector("#workspace-history-list") as HTMLElement;
  const historyQuery = document.querySelector("#workspace-history-query") as HTMLInputElement;
  const historyAuthor = document.querySelector("#workspace-history-author") as HTMLInputElement;
  const historyRef = document.querySelector("#workspace-history-ref") as HTMLSelectElement;
  const historyFilterBtn = document.querySelector("#workspace-history-filter") as HTMLButtonElement;
  const workspaceMain = document.querySelector(".ws-main") as HTMLElement;
  const previewResizer = document.querySelector("#workspace-preview-resizer") as HTMLElement;
  const previewTitle = document.querySelector("#workspace-preview-title") as HTMLButtonElement;
  const previewActions = document.querySelector("#workspace-preview-actions") as HTMLElement;
  const previewNotice = document.querySelector("#workspace-preview-notice") as HTMLElement;
  const previewImage = document.querySelector("#workspace-preview-image") as HTMLImageElement;
  const markdownPreview = document.querySelector("#workspace-markdown-preview") as HTMLElement;
  const previewCode = document.querySelector("#workspace-preview-code") as HTMLElement;

  let project: string | null = null;
  let mode: WorkspaceMode = "files";
  let opened = false;
  let projectRevision = 0;
  let refreshCount = 0;
  let refreshTimer: number | null = null;
  let resizing = false;
  let previewResizing = false;
  let layoutFrame: number | null = null;
  let refreshVisualTimer: number | null = null;
  let lastGitFingerprint: string | null = null;
  let gitRefreshSequence = 0;
  let historyCommits: GitCommit[] = [];
  let historyLoading = false;
  let historyHasMore = true;
  let historyRequestSequence = 0;
  let appliedHistoryFilters = historyFilterArgs("", "", "");
  let stagedCount = 0;
  let commitExpanded = false;
  let repositoryAhead = 0;
  let repositoryBehind = 0;
  let gitOperationRunning = false;
  let commitMenu: HTMLElement | null = null;
  let stagedPaths: string[] = [];
  let unstagedPaths: string[] = [];
  let previewCollapsed = workspacePreviewStartsCollapsed(mode);
  let markdownDisplayMode: "preview" | "source" = "preview";
  let markdownSource: string | null = null;
  let markdownSourcePath = "";
  let markdownRenderedHtml = "";
  let developerDisplayMode: "preview" | "source" = "preview";
  let developerSource: string | null = null;
  let developerSourcePath = "";
  let activeDeveloperPreviewKind: DeveloperPreviewKind = null;
  let developerRenderer: (() => void) | null = null;
  const directoryObservers = new Set<IntersectionObserver>();

  panel.classList.add("files-mode");

  function widthStorageMode(value: WorkspaceMode): keyof typeof WIDTH_STORAGE_KEYS {
    if (value === "shell") return "shell";
    return value === "files" ? "files" : "git";
  }

  function constrainedPanelWidth(requested: number, value: WorkspaceMode): number {
    const width = workspacePanelWidth(requested);
    if (value === "shell") {
      return Math.min(width, Math.max(280, Math.floor(window.innerWidth * 0.6)));
    }
    return width;
  }

  function restorePanelWidth(value: WorkspaceMode) {
    const widthMode = widthStorageMode(value);
    const stored = localStorage.getItem(WIDTH_STORAGE_KEYS[widthMode]);
    const legacy = localStorage.getItem(LEGACY_WIDTH_STORAGE_KEY);
    const width = widthMode === "shell"
      ? storedWorkspacePanelWidth(stored, 440)
      : storedWorkspacePanelWidth(stored ?? legacy, 360);
    panel.style.width = `${constrainedPanelWidth(width, value)}px`;
  }

  restorePanelWidth(mode);
  const storedPreviewHeight = Number(localStorage.getItem(PREVIEW_HEIGHT_STORAGE_KEY));
  if (Number.isFinite(storedPreviewHeight) && storedPreviewHeight >= 140) {
    workspaceMain.style.setProperty("--workspace-preview-height", `${storedPreviewHeight}px`);
  }

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

  function clearDirectoryObservers() {
    for (const observer of directoryObservers) observer.disconnect();
    directoryObservers.clear();
  }

  function clearPreview() {
    resetMarkdownPreview();
    resetDeveloperPreview();
    previewTitle.textContent = t("workspace_preview");
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    previewImage.hidden = true;
    previewImage.removeAttribute("src");
    markdownPreview.hidden = true;
    markdownPreview.innerHTML = "";
    previewCode.hidden = false;
    previewCode.textContent = "";
    previewActions.innerHTML = "";
  }

  function resetMarkdownPreview() {
    markdownDisplayMode = "preview";
    markdownSource = null;
    markdownSourcePath = "";
    markdownRenderedHtml = "";
    markdownPreview.hidden = true;
    markdownPreview.innerHTML = "";
  }

  function resetDeveloperPreview() {
    developerDisplayMode = "preview";
    developerSource = null;
    developerSourcePath = "";
    activeDeveloperPreviewKind = null;
    developerRenderer = null;
    markdownPreview.classList.remove("ws-developer-preview");
  }

  function renderDeveloperActions(enabled: boolean) {
    previewActions.innerHTML = "";
    const group = document.createElement("div");
    group.className = "ws-markdown-toggle";
    for (const value of ["preview", "source"] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ws-small-btn";
      button.textContent = t(
        value === "preview" ? "workspace_markdown_preview" : "workspace_markdown_source",
      );
      button.disabled = !enabled;
      button.classList.toggle("active", developerDisplayMode === value);
      button.onclick = () => setDeveloperDisplayMode(value);
      group.appendChild(button);
    }
    previewActions.appendChild(group);
  }

  function setDeveloperDisplayMode(value: "preview" | "source") {
    if (developerSource === null) return;
    developerDisplayMode = value;
    renderDeveloperActions(true);
    markdownPreview.innerHTML = "";
    if (value === "preview") {
      if (activeDeveloperPreviewKind === "diff") {
        markdownPreview.hidden = true;
        previewCode.hidden = false;
        renderHighlightedDiff(developerSource, developerSourcePath);
      } else {
        previewCode.hidden = true;
        markdownPreview.hidden = false;
        developerRenderer?.();
      }
    } else {
      markdownPreview.hidden = true;
      previewCode.hidden = false;
      renderHighlightedText(developerSource, developerSourcePath);
    }
  }

  function prepareDeveloperPreview(content: string, path: string, kind: Exclude<DeveloperPreviewKind, null>): boolean {
    markdownPreview.classList.add("ws-developer-preview");
    let renderer: () => void;
    try {
      if (kind === "table") {
        const delimiter = path.toLowerCase().endsWith(".tsv") ? "\t" : ",";
        const tablePreview = parseDelimitedPreview(content, delimiter);
        renderer = () => {
          const wrapper = document.createElement("div");
          wrapper.className = "ws-table-wrap";
          const table = document.createElement("table");
          const head = document.createElement("thead");
          const headRow = document.createElement("tr");
          for (const value of tablePreview.headers) { const cell = document.createElement("th"); cell.textContent = value; headRow.appendChild(cell); }
          head.appendChild(headRow); table.appendChild(head);
          const body = document.createElement("tbody");
          for (const row of tablePreview.rows) { const tr = document.createElement("tr"); for (const value of row) { const cell = document.createElement("td"); cell.textContent = value; tr.appendChild(cell); } body.appendChild(tr); }
          table.appendChild(body); wrapper.appendChild(table); markdownPreview.appendChild(wrapper);
        };
      } else if (kind === "svg") {
        const safe = sanitizeSvgPreview(content);
        renderer = () => { const frame = document.createElement("div"); frame.className = "ws-svg-preview"; frame.innerHTML = safe; markdownPreview.appendChild(frame); };
      } else {
        renderer = () => {};
      }
    } catch {
      return false;
    }
    developerDisplayMode = "preview";
    developerSource = content;
    developerSourcePath = path;
    activeDeveloperPreviewKind = kind;
    developerRenderer = renderer;
    setDeveloperDisplayMode("preview");
    return true;
  }

  function renderMarkdownActions(enabled: boolean) {
    previewActions.innerHTML = "";
    const group = document.createElement("div");
    group.className = "ws-markdown-toggle";
    for (const value of ["preview", "source"] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ws-small-btn";
      button.textContent = t(
        value === "preview" ? "workspace_markdown_preview" : "workspace_markdown_source",
      );
      button.disabled = !enabled;
      button.classList.toggle("active", markdownDisplayMode === value);
      button.onclick = () => setMarkdownDisplayMode(value);
      group.appendChild(button);
    }
    previewActions.appendChild(group);
  }

  function setMarkdownDisplayMode(value: "preview" | "source") {
    if (markdownSource === null) return;
    markdownDisplayMode = value;
    renderMarkdownActions(true);
    if (value === "preview") {
      previewCode.hidden = true;
      markdownPreview.innerHTML = markdownRenderedHtml;
      markdownPreview.hidden = false;
    } else {
      markdownPreview.hidden = true;
      markdownPreview.innerHTML = "";
      previewCode.hidden = false;
      renderHighlightedText(markdownSource, markdownSourcePath);
    }
  }

  function setPreviewCollapsed(collapsed: boolean) {
    previewCollapsed = collapsed;
    workspaceMain.classList.toggle("preview-collapsed", collapsed);
    previewTitle.setAttribute("aria-expanded", String(!collapsed));
  }

  function setRefreshVisual(state: "running" | "complete" | "error") {
    if (refreshVisualTimer !== null) {
      window.clearTimeout(refreshVisualTimer);
      refreshVisualTimer = null;
    }
    panel.classList.toggle("refreshing", state === "running");
    panel.classList.toggle("refreshed", state === "complete");
    panel.classList.toggle("refresh-error", state === "error");
    refreshProgress.setAttribute(
      "aria-label",
      t(
        state === "running"
          ? "workspace_refreshing"
          : state === "complete"
            ? "workspace_refresh_complete"
            : "workspace_refresh_failed",
      ),
    );
    if (state !== "running") {
      refreshVisualTimer = window.setTimeout(() => {
        panel.classList.remove("refreshed");
        panel.classList.remove("refresh-error");
        refreshProgress.removeAttribute("aria-label");
        refreshVisualTimer = null;
      }, 700);
    }
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

  function renderHighlightedDiff(content: string, sourcePath: string) {
    previewCode.innerHTML = "";
    let activePath = sourcePath;
    for (const line of content.split("\n")) {
      const nextPath = line.match(/^\+\+\+ b\/(.+)$/)?.[1];
      if (nextPath) activePath = nextPath;
      const rendered = tokenizeDiffLine(line, activePath);
      const lineEl = document.createElement("span");
      lineEl.className = `ws-diff-line ${rendered.kind}`;
      if (rendered.prefix) {
        const prefix = document.createElement("span");
        prefix.className = "ws-diff-prefix";
        prefix.textContent = rendered.prefix;
        lineEl.appendChild(prefix);
      }
      for (const token of rendered.tokens) {
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
    resetMarkdownPreview();
    resetDeveloperPreview();
    previewActions.innerHTML = "";
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
    if (!diff && preview.kind === "text" && isMarkdownPreviewPath(sourcePath)) {
      markdownSource = preview.content;
      markdownSourcePath = sourcePath;
      if (canRenderMarkdown(preview.size_bytes)) {
        markdownRenderedHtml = renderSafeMarkdown(preview.content);
        setMarkdownDisplayMode("preview");
      } else {
        markdownDisplayMode = "source";
        renderMarkdownActions(false);
        previewNotice.textContent = t("workspace_markdown_too_large");
        previewNotice.classList.add("show");
        renderHighlightedText(preview.content, sourcePath);
      }
      return;
    }
    if (!diff && preview.kind === "text") {
      const kind = developerPreviewKind(sourcePath);
      if (kind && prepareDeveloperPreview(preview.content, sourcePath, kind)) return;
    }
    if (!diff) {
      renderHighlightedText(preview.content, sourcePath);
      return;
    }
    renderHighlightedDiff(preview.content, sourcePath);
  }

  function showPreviewLoading(title: string, sourcePath = title) {
    resetMarkdownPreview();
    resetDeveloperPreview();
    previewActions.innerHTML = "";
    setPreviewCollapsed(false);
    previewTitle.textContent = title;
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    previewImage.hidden = true;
    previewImage.removeAttribute("src");
    previewCode.hidden = false;
    previewCode.textContent = t("workspace_loading");
    if (isMarkdownPreviewPath(sourcePath)) renderMarkdownActions(false);
    else if (developerPreviewKind(sourcePath)) renderDeveloperActions(false);
  }

  function renderLoadError(container: HTMLElement, error: unknown) {
    renderState(container, t("workspace_load_failed", String(error)));
  }

  async function previewFile(relativePath: string, title = relativePath) {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    showPreviewLoading(title, relativePath);
    try {
      const preview = await invoke<FilePreview>("read_workspace_file", {
        project: activeProject,
        relativePath,
        maxBytes: workspaceLocalPreviewLimit(relativePath),
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
    append = false,
  ) {
    if (!append) container.innerHTML = "";
    if (!append && entries.length === 0) {
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
      const metadata = document.createElement("span");
      metadata.className = "ws-tree-meta";
      const size = formatWorkspaceEntrySize(entry.size_bytes, entry.is_dir);
      const modified = formatWorkspaceModifiedTime(entry.modified_unix_seconds);
      metadata.textContent = [size, modified].filter(Boolean).join(" · ");
      row.append(arrow);
      if (decoration.icon) row.append(icon);
      row.append(name);
      if (metadata.textContent) row.append(metadata);
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
            const ok = await loadDirectoryPage(
              entry.relative_path,
              children,
              depth + 1,
              activeProject,
              revision,
              0,
              false,
            );
            if (project !== activeProject || projectRevision !== revision) return;
            loaded = ok;
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

  async function loadDirectoryPage(
    relativePath: string,
    container: HTMLElement,
    depth: number,
    activeProject: string,
    revision: number,
    offset: number,
    append: boolean,
  ): Promise<boolean> {
    try {
      const page = await invoke<WorkspaceEntriesPage>("list_workspace_entries", {
        project: activeProject,
        relativePath,
        offset,
        limit: 100,
      });
      if (project !== activeProject || projectRevision !== revision) return true;
      container.querySelector(":scope > .ws-directory-more")?.remove();
      container.querySelector(":scope > .ws-directory-cap")?.remove();
      renderTreeEntries(page.entries, container, depth, activeProject, revision, append);
      if (page.has_more) {
        const more = document.createElement("button");
        more.type = "button";
        more.className = "ws-directory-more";
        more.textContent = t("workspace_load_more");
        const loadNext = () => {
          if (more.disabled) return;
          more.disabled = true;
          more.textContent = t("workspace_loading");
          observer?.disconnect();
          if (observer) directoryObservers.delete(observer);
          void loadDirectoryPage(
            relativePath,
            container,
            depth,
            activeProject,
            revision,
            offset + page.entries.length,
            true,
          );
        };
        more.onclick = loadNext;
        const observer = new IntersectionObserver(
          (entries) => {
            if (entries.some((entry) => entry.isIntersecting)) loadNext();
          },
          { root: filesView, rootMargin: "120px 0px" },
        );
        directoryObservers.add(observer);
        observer.observe(more);
        container.appendChild(more);
      } else if (page.capped) {
        const cap = document.createElement("div");
        cap.className = "ws-directory-cap";
        cap.textContent = t("workspace_directory_capped");
        container.appendChild(cap);
      }
      return true;
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return true;
      renderLoadError(container, error);
      return false;
    }
  }

  async function refreshFiles(): Promise<boolean> {
    if (!project) return false;
    const activeProject = project;
    const revision = projectRevision;
    clearDirectoryObservers();
    renderState(fileTree, t("workspace_loading"));
    return loadDirectoryPage("", fileTree, 0, activeProject, revision, 0, false);
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
      previewActions.innerHTML = "";
      if (preview.kind === "text") {
        const file = parseUnifiedDiff(preview.content)[0];
        const rawLines = preview.content.split("\n");
        const renderedLines = Array.from(previewCode.children) as HTMLElement[];
        let searchFrom = 0;
        file?.hunks.forEach((hunk, index) => {
          const button = document.createElement("button");
          button.type = "button";
          button.className = "ws-small-btn";
          button.textContent = change.staged
            ? t("workspace_hunk_unstage", index + 1)
            : t("workspace_hunk_stage", index + 1);
          button.onclick = () =>
            void runGitMutation(
              change.staged ? t("workspace_unstage") : t("workspace_stage"),
              "git_apply_patch",
              {
                patch: hunk.patch,
                cached: true,
                reverse: change.staged,
              },
            );
          previewActions.appendChild(button);
          const selected = new Set<number>();
          const lineButton = document.createElement("button");
          lineButton.type = "button";
          lineButton.className = "ws-small-btn";
          lineButton.textContent = t("workspace_selected_lines", index + 1);
          lineButton.disabled = true;
          const headerIndex = rawLines.findIndex(
            (line, lineIndex) => lineIndex >= searchFrom && line === hunk.header,
          );
          searchFrom = Math.max(searchFrom, headerIndex + hunk.lines.length + 1);
          hunk.lines.forEach((line, lineIndex) => {
            if (!line.startsWith("+") && !line.startsWith("-")) return;
            const element = renderedLines[headerIndex + lineIndex + 1];
            if (!element) return;
            element.classList.add("selectable");
            element.onclick = () => {
              if (selected.has(lineIndex)) selected.delete(lineIndex);
              else selected.add(lineIndex);
              element.classList.toggle("selected", selected.has(lineIndex));
              lineButton.disabled = selected.size === 0;
            };
          });
          lineButton.onclick = () => {
            const patch = buildSelectedLinesPatch(file.header, hunk.header, hunk.lines, selected);
            if (patch) {
              void runGitMutation(
                change.staged ? t("workspace_unstage") : t("workspace_stage"),
                "git_apply_patch",
                { patch, cached: true, reverse: change.staged },
              );
            }
          };
          previewActions.appendChild(lineButton);
        });
      }
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      previewCode.textContent = t("workspace_load_failed", String(error));
    }
  }

  function setOperation(message: string, show = true) {
    operationEl.className = "ws-operation";
    operationEl.textContent = message;
    operationEl.classList.toggle("show", show && !!message);
  }

  function remoteOperationCommitCount(operation: "fetch" | "pull" | "push") {
    if (operation === "push") return repositoryAhead;
    if (operation === "pull") return repositoryBehind;
    return 0;
  }

  function remoteOperationLabel(operation: "fetch" | "pull" | "push", count: number) {
    if (operation === "fetch") return "Fetch";
    const labels = gitSyncButtonLabels(
      operation === "push" ? count : 0,
      operation === "pull" ? count : 0,
    );
    return operation === "push" ? labels.push : labels.pull;
  }

  function normalizedGitOutput(output: string) {
    return output.replace(/\r\n?/g, "\n").trim();
  }

  function renderRemoteOperation(
    state: "running" | "success" | "error",
    operation: "fetch" | "pull" | "push",
    count: number,
    output = "",
  ) {
    const label = remoteOperationLabel(operation, count);
    const command = `$ git ${operation} --progress`;
    const status =
      state === "running"
        ? t("workspace_remote_running", label)
        : state === "success"
          ? t("workspace_remote_success", label)
          : t("workspace_remote_error", label);
    const fallback =
      state === "running"
        ? t("workspace_remote_connecting")
        : state === "success"
          ? t("workspace_remote_no_output")
          : output;

    operationEl.className = `ws-operation show ${state}`;
    operationEl.innerHTML = "";
    const head = document.createElement("div");
    head.className = "ws-operation-head";
    const icon = document.createElement("span");
    icon.className = "ws-operation-icon";
    icon.textContent = state === "running" ? "●" : state === "success" ? "✓" : "×";
    const title = document.createElement("span");
    title.className = "ws-operation-title";
    title.textContent = status;
    head.append(icon, title);
    if (count > 0) {
      const detail = document.createElement("span");
      detail.className = "ws-operation-detail";
      detail.textContent = t("workspace_remote_commits", count);
      head.appendChild(detail);
    }
    const progress = document.createElement("div");
    progress.className = "ws-operation-progress";
    const bar = document.createElement("div");
    bar.className = "ws-operation-progress-bar";
    progress.appendChild(bar);
    const log = document.createElement("pre");
    log.className = "ws-operation-log";
    log.textContent = `${command}\n\n${normalizedGitOutput(output) || fallback}`;
    operationEl.append(head, progress, log);
    log.scrollTop = log.scrollHeight;
  }

  function updateGitControls() {
    const controls = gitOperationControls(gitOperationRunning);
    fetchBtn.disabled = controls.fetchDisabled;
    pullBtn.disabled = controls.pullDisabled;
    pushBtn.disabled = controls.pushDisabled;
    branchSelect.disabled = controls.branchDisabled;
    stageAllBtn.disabled = gitOperationRunning || unstagedPaths.length === 0;
    unstageAllBtn.disabled = gitOperationRunning || stagedPaths.length === 0;
    const enabled = canCommit(commitMessage.value, stagedCount, amendInput.checked);
    commitBtn.disabled = gitOperationRunning || !enabled;
    commitPushBtn.disabled = gitOperationRunning || !enabled;
    updateCommitPresentation();
  }

  function updateCommitPresentation() {
    if (stagedCount === 0) commitExpanded = false;
    const presentation = workspaceCommitPresentation(stagedCount, commitExpanded);
    commitLauncher.hidden = !presentation.showLauncher;
    commitBox.hidden = !presentation.showForm;
  }

  async function runGitMutation(
    label: string,
    command: string,
    args: Record<string, unknown> = {},
  ): Promise<boolean> {
    if (!project || gitOperationRunning) return false;
    const activeProject = project;
    const inlineFeedback = gitMutationUsesInlineFeedback(command);
    gitOperationRunning = true;
    updateGitControls();
    if (!inlineFeedback) setOperation(t("workspace_operation_running", label));
    try {
      const result = await invoke<string | void>(command, { project: activeProject, ...args });
      if (project !== activeProject) return false;
      if (!inlineFeedback) {
        setOperation(typeof result === "string" && result ? result : label);
      }
      await refreshGit(true);
      return true;
    } catch (error) {
      if (project === activeProject) setOperation(String(error));
      return false;
    } finally {
      gitOperationRunning = false;
      updateGitControls();
    }
  }

  async function runHistoryMutation(
    label: string,
    command: string,
    args: Record<string, unknown> = {},
  ): Promise<boolean> {
    const ok = await runGitMutation(label, command, args);
    if (ok) await refreshHistory(true);
    return ok;
  }

  function rowAction(label: string, icon: string, danger: boolean, action: () => void) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `ws-row-action${danger ? " danger" : ""}`;
    button.title = label;
    button.textContent = icon;
    button.onclick = (event) => {
      event.stopPropagation();
      action();
    };
    return button;
  }

  function gitChangeKey(change: GitDisplayChange) {
    return `${change.staged ? "staged" : "unstaged"}:${change.path}`;
  }

  function buildGitRow(change: GitDisplayChange) {
    const row = document.createElement("div");
    row.className = "ws-git-row";
    row.dataset.changeKey = gitChangeKey(change);
    row.title = change.path;
    const main = document.createElement("button");
    main.type = "button";
    main.className = "ws-git-row-main";
    const status = document.createElement("span");
    status.className = "ws-git-status";
    status.textContent = change.status;
    const path = document.createElement("span");
    path.className = "ws-git-path";
    path.textContent = change.path;
    main.append(status, path);
    main.onclick = () => void previewGitChange(change);
    const actions = document.createElement("span");
    actions.className = "ws-row-actions";
    if (change.staged) {
      actions.appendChild(
        rowAction(t("workspace_unstage"), "−", false, () => {
          void runGitMutation(t("workspace_unstage"), "git_unstage_paths", {
            paths: [change.path],
          });
        }),
      );
    } else {
      actions.appendChild(
        rowAction(t("workspace_stage"), "+", false, () => {
          void runGitMutation(t("workspace_stage"), "git_stage_paths", {
            paths: [change.path],
          });
        }),
      );
      actions.appendChild(
        rowAction(t("workspace_discard"), "×", true, () => {
          void confirm(t("workspace_discard_confirm", change.path)).then((ok) => {
            if (ok) {
              void runGitMutation(t("workspace_discard"), "git_discard_paths", {
                paths: [change.path],
              });
            }
          });
        }),
      );
    }
    row.append(main, actions);
    return row;
  }

  function gitSection(kind: "staged" | "unstaged", title: string) {
    let section = gitList.querySelector<HTMLElement>(`[data-git-section="${kind}"]`);
    if (!section) {
      section = document.createElement("section");
      section.className = "ws-git-section";
      section.dataset.gitSection = kind;
      const heading = document.createElement("div");
      heading.className = "ws-git-section-title";
      const rows = document.createElement("div");
      rows.className = "ws-git-section-rows";
      section.append(heading, rows);
    }
    const heading = section.querySelector(".ws-git-section-title") as HTMLElement;
    heading.dataset.title = title;
    return section;
  }

  function reconcileGitSections(
    staged: GitDisplayChange[],
    unstaged: GitDisplayChange[],
  ) {
    gitList.querySelector(".ws-state")?.remove();
    const nextChanges = [...staged, ...unstaged];
    const nextByKey = new Map(nextChanges.map((change) => [gitChangeKey(change), change]));
    const existingRows = Array.from(
      gitList.querySelectorAll<HTMLElement>(".ws-git-row[data-change-key]"),
    );
    const existingByKey = new Map(
      existingRows.map((row) => [row.dataset.changeKey as string, row]),
    );
    const changes = reconcileGitChangeKeys(
      [...existingByKey.keys()],
      [...nextByKey.keys()],
    );

    for (const key of changes.removed) {
      const row = existingByKey.get(key);
      if (!row) continue;
      row.classList.add("ws-row-leave");
      window.setTimeout(() => row.remove(), 140);
    }

    const stagedSection = gitSection("staged", t("workspace_staged"));
    const unstagedSection = gitSection("unstaged", t("workspace_unstaged"));
    gitList.append(stagedSection, unstagedSection);
    for (const [section, items] of [
      [stagedSection, staged],
      [unstagedSection, unstaged],
    ] as const) {
      const heading = section.querySelector(".ws-git-section-title") as HTMLElement;
      const rows = section.querySelector(".ws-git-section-rows") as HTMLElement;
      heading.textContent = `${heading.dataset.title} (${items.length})`;
      section.hidden = items.length === 0;
      for (const item of items) {
        const key = gitChangeKey(item);
        let row = existingByKey.get(key);
        if (!row) {
          row = buildGitRow(item);
          row.classList.add("ws-row-enter");
          row.addEventListener("animationend", () => row?.classList.remove("ws-row-enter"), {
            once: true,
          });
        } else {
          const status = row.querySelector(".ws-git-status");
          if (status) status.textContent = item.status;
          const main = row.querySelector(".ws-git-row-main") as HTMLButtonElement | null;
          if (main) main.onclick = () => void previewGitChange(item);
        }
        rows.appendChild(row);
      }
    }
  }

  async function refreshGit(background = false): Promise<boolean> {
    if (!project) return false;
    const activeProject = project;
    const revision = projectRevision;
    const sequence = ++gitRefreshSequence;
    if (!background && lastGitFingerprint === null) {
      renderState(gitList, t("workspace_loading"));
      gitMeta.textContent = "";
    }
    try {
      const [status, summary, refs] = await Promise.all([
        invoke<GitStatus>("git_workspace_status", { project: activeProject }),
        invoke<RepositorySummary>("git_repository_summary", { project: activeProject }),
        invoke<GitRef[]>("git_list_refs", { project: activeProject }),
      ]);
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== gitRefreshSequence
      ) return true;
      const fingerprint = JSON.stringify([gitStatusFingerprint(status), summary, refs]);
      if (fingerprint === lastGitFingerprint) return true;
      lastGitFingerprint = fingerprint;
      if (!status.is_repo) {
        stagedPaths = [];
        unstagedPaths = [];
        stagedCount = 0;
        updateGitControls();
        repositoryAhead = 0;
        repositoryBehind = 0;
        gitMeta.textContent = "";
        const labels = gitSyncButtonLabels(0, 0);
        pullBtn.textContent = labels.pull;
        pushBtn.textContent = labels.push;
        renderState(gitList, t("workspace_not_repo"));
        return true;
      }
      repositoryAhead = summary.ahead;
      repositoryBehind = summary.behind;
      gitMeta.textContent = `⎇ ${summary.branch ?? "DETACHED"}${summary.upstream ? ` · ${summary.upstream}` : ""} · ↑${summary.ahead} ↓${summary.behind}${summary.operation ? ` · ${summary.operation}` : ""}`;
      const labels = gitSyncButtonLabels(summary.ahead, summary.behind);
      pullBtn.textContent = labels.pull;
      pushBtn.textContent = labels.push;
      branchSelect.innerHTML = "";
      historyRef.innerHTML = `<option value="">${t("workspace_all_refs")}</option>`;
      for (const kind of ["local", "remote"] as const) {
        const group = document.createElement("optgroup");
        group.label = kind === "local" ? "Local" : "Remote";
        for (const ref of refs.filter((item) => item.kind === kind)) {
          const option = document.createElement("option");
          option.value = ref.name;
          option.textContent = ref.name;
          option.selected = ref.current;
          option.dataset.kind = ref.kind;
          group.appendChild(option);
        }
        if (group.children.length > 0) branchSelect.appendChild(group);
      }
      for (const ref of refs) {
        if (ref.kind !== "tag") {
          const option = document.createElement("option");
          option.value = ref.name;
          option.textContent = ref.name;
          historyRef.appendChild(option);
        }
      }
      const sections = groupGitChanges(status.changes);
      stagedPaths = [...new Set(sections.staged.map((change) => change.path))];
      unstagedPaths = [...new Set(sections.unstaged.map((change) => change.path))];
      stagedCount = stagedPaths.length;
      updateGitControls();
      if (sections.staged.length === 0 && sections.unstaged.length === 0) {
        renderState(gitList, t("workspace_no_changes"));
        return true;
      }
      reconcileGitSections(sections.staged, sections.unstaged);
      return true;
    } catch (error) {
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== gitRefreshSequence
      ) return true;
      if (background) {
        console.error("workspace git refresh failed", error);
        return false;
      }
      renderLoadError(gitList, error);
      return false;
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

  function closeCommitMenu() {
    commitMenu?.remove();
    commitMenu = null;
  }

  function openCommitMenu(commit: GitCommit, x: number, y: number) {
    closeCommitMenu();
    const menu = document.createElement("div");
    menu.className = "ws-commit-menu";
    menu.style.left = `${Math.min(x, window.innerWidth - 180)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - 300)}px`;
    const add = (label: string, danger: boolean, action: () => void) => {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = label;
      button.classList.toggle("danger", danger);
      button.onclick = () => {
        closeCommitMenu();
        action();
      };
      menu.appendChild(button);
    };
    add(t("workspace_copy_hash"), false, () => {
      void navigator.clipboard?.writeText(commit.hash);
    });
    add(t("workspace_new_branch"), false, () => {
      const name = window.prompt(t("workspace_create_branch_prompt"));
      if (name) {
        void runHistoryMutation(t("workspace_new_branch"), "git_create_branch", {
          name,
          startPoint: commit.hash,
          checkout: true,
        });
      }
    });
    add("Tag", false, () => {
      const name = window.prompt(t("workspace_create_tag_prompt"));
      if (name) {
        void runHistoryMutation("Tag", "git_create_tag", { name, target: commit.hash });
      }
    });
    add(t("workspace_cherry_pick"), false, () => {
      void runHistoryMutation(t("workspace_cherry_pick"), "git_commit_action", {
        action: "cherry_pick",
        commit: commit.hash,
      });
    });
    add(t("workspace_revert"), true, () => {
      void confirm(t("workspace_danger_confirm", t("workspace_revert"))).then((ok) => {
        if (ok) {
          void runHistoryMutation(t("workspace_revert"), "git_commit_action", {
            action: "revert",
            commit: commit.hash,
          });
        }
      });
    });
    for (const mode of ["soft", "mixed", "hard"] as const) {
      const label = t(`workspace_reset_${mode}`);
      add(label, mode === "hard", () => {
        void confirm(t("workspace_danger_confirm", label)).then((ok) => {
          if (ok) {
            void runHistoryMutation(label, "git_reset_to", { commit: commit.hash, mode });
          }
        });
      });
    }
    document.body.appendChild(menu);
    commitMenu = menu;
  }

  function appendHistoryRows(commits: GitCommit[]) {
    for (const commit of commits) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-history-row";
      row.title = commit.subject;
      const hash = document.createElement("span");
      hash.className = "ws-history-graph";
      hash.textContent = commit.parents.length > 1 ? "◆" : "●";
      hash.title = commit.short_hash;
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
      if (commit.refs.length > 0) {
        const refs = document.createElement("span");
        refs.className = "ws-history-refs";
        for (const value of commit.refs) {
          const badge = document.createElement("span");
          badge.className = "ws-history-ref-badge";
          badge.textContent = value;
          refs.appendChild(badge);
        }
        body.appendChild(refs);
      }
      row.append(hash, body);
      row.onclick = () => void previewCommit(commit);
      row.oncontextmenu = (event) => {
        event.preventDefault();
        openCommitMenu(commit, event.clientX, event.clientY);
      };
      historyList.appendChild(row);
    }
  }

  async function refreshHistory(reset = true): Promise<boolean> {
    if (!project) return false;
    if (historyLoading || (!reset && !historyHasMore)) return true;
    const activeProject = project;
    const revision = projectRevision;
    const sequence = ++historyRequestSequence;
    historyLoading = true;
    if (reset) {
      appliedHistoryFilters = historyFilterArgs(
        historyQuery.value,
        historyAuthor.value,
        historyRef.value,
      );
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
        if (
          project !== activeProject ||
          projectRevision !== revision ||
          sequence !== historyRequestSequence
        ) return true;
        if (!status.is_repo) {
          historyHasMore = false;
          renderState(historyList, t("workspace_not_repo"));
          return true;
        }
      }
      const page = gitHistoryPage(historyCommits.length);
      const commits = await invoke<GitCommit[]>("git_workspace_history", {
        project: activeProject,
        skip: page.skip,
        limit: page.limit,
        ...appliedHistoryFilters,
      });
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== historyRequestSequence
      ) return true;
      if (reset) historyList.innerHTML = "";
      historyCommits.push(...commits);
      historyHasMore = commits.length === page.limit;
      if (historyCommits.length === 0) {
        renderState(historyList, t("workspace_history_empty"));
      } else {
        appendHistoryRows(commits);
      }
      return true;
    } catch (error) {
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== historyRequestSequence
      ) return true;
      renderLoadError(historyList, error);
      return false;
    } finally {
      if (sequence === historyRequestSequence) historyLoading = false;
    }
  }

  async function refresh(background = false) {
    if (!opened || !project || mode === "shell") return;
    if (!background) {
      refreshCount += 1;
      refreshBtn.disabled = true;
      setRefreshVisual("running");
    }
    let success = true;
    try {
      if (mode === "files") success = await refreshFiles();
      else if (mode === "git") success = await refreshGit(background);
      else success = await refreshHistory(true);
    } finally {
      if (!background) {
        refreshCount -= 1;
        refreshBtn.disabled = refreshCount > 0;
        if (refreshCount === 0) {
          if (success) setRefreshVisual("complete");
          else setRefreshVisual("error");
        }
      }
    }
  }

  function startRefreshTimer() {
    if (refreshTimer !== null) window.clearInterval(refreshTimer);
    refreshTimer = window.setInterval(() => {
      if (opened && mode === "git" && !gitOperationRunning) void refresh(true);
    }, 3000);
  }

  function stopRefreshTimer() {
    if (refreshTimer === null) return;
    window.clearInterval(refreshTimer);
    refreshTimer = null;
  }

  function syncOpenModeClasses() {
    stage.classList.toggle("workspace-files-open", opened && mode === "files");
    stage.classList.toggle("workspace-git-open", opened && (mode === "git" || mode === "history"));
    stage.classList.toggle("workspace-shell-open", opened && mode === "shell");
  }

  function setOpened(next: boolean) {
    opened = next && !!project;
    panel.classList.toggle("open", opened);
    stage.classList.toggle("workspace-open", opened);
    syncOpenModeClasses();
    panel.setAttribute("aria-hidden", String(!opened));
    resizer.setAttribute("aria-hidden", String(!opened));
    if (opened && mode !== "shell") {
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
    const shellActive = mode === "shell";
    panel.classList.toggle("files-mode", filesActive);
    panel.classList.toggle("git-mode", gitActive || historyActive);
    panel.classList.toggle("shell-mode", shellActive);
    gitTab.classList.toggle("active", gitActive);
    gitTab.setAttribute("aria-selected", String(gitActive));
    historyTab.classList.toggle("active", historyActive);
    historyTab.setAttribute("aria-selected", String(historyActive));
    filesView.hidden = !filesActive;
    gitView.hidden = !gitActive;
    historyView.hidden = !historyActive;
    restorePanelWidth(mode);
    onModeChange(mode);
    syncOpenModeClasses();
    if (shellActive) {
      stopRefreshTimer();
    } else {
      clearPreview();
      setPreviewCollapsed(mode === "shell" ? false : workspacePreviewStartsCollapsed(mode));
      if (opened) {
        startRefreshTimer();
        void refresh();
      }
    }
  }

  closeBtn.onclick = () => {
    setOpened(false);
    onClose();
  };
  refreshBtn.onclick = () => void refresh();
  gitTab.onclick = () => setMode("git");
  historyTab.onclick = () => setMode("history");
  previewTitle.onclick = () => setPreviewCollapsed(!previewCollapsed);
  previewResizer.addEventListener("pointerdown", (event) => {
    if (previewCollapsed) return;
    event.preventDefault();
    previewResizing = true;
    document.body.classList.add("ws-preview-resizing");
    previewResizer.setPointerCapture(event.pointerId);
  });
  previewResizer.addEventListener("pointermove", (event) => {
    if (!previewResizing) return;
    const bounds = workspaceMain.getBoundingClientRect();
    const maximum = Math.max(140, bounds.height - 105);
    const height = Math.min(maximum, Math.max(140, bounds.bottom - event.clientY));
    workspaceMain.style.setProperty("--workspace-preview-height", `${height}px`);
    scheduleLayoutChange();
  });
  const finishPreviewResize = (event: PointerEvent) => {
    if (!previewResizing) return;
    previewResizing = false;
    document.body.classList.remove("ws-preview-resizing");
    if (previewResizer.hasPointerCapture(event.pointerId)) {
      previewResizer.releasePointerCapture(event.pointerId);
    }
    const height = workspaceMain.style.getPropertyValue("--workspace-preview-height");
    if (height) localStorage.setItem(PREVIEW_HEIGHT_STORAGE_KEY, height.replace("px", ""));
    scheduleLayoutChange();
  };
  previewResizer.addEventListener("pointerup", finishPreviewResize);
  previewResizer.addEventListener("pointercancel", finishPreviewResize);
  stageAllBtn.onclick = () => {
    if (unstagedPaths.length > 0) {
      void runGitMutation(t("workspace_stage_all"), "git_stage_paths", {
        paths: unstagedPaths,
      });
    }
  };
  unstageAllBtn.onclick = () => {
    if (stagedPaths.length > 0) {
      void runGitMutation(t("workspace_unstage_all"), "git_unstage_paths", {
        paths: stagedPaths,
      });
    }
  };
  branchSelect.onchange = () => {
    const reference = branchSelect.value;
    const option = branchSelect.selectedOptions[0];
    if (reference) {
      void runGitMutation(reference, "git_checkout_ref", {
        reference,
        track: option?.dataset.kind === "remote",
      }).then((ok) => {
        if (ok) void refreshHistory(true);
      });
    }
  };
  const remoteOperation = async (operation: "fetch" | "pull" | "push") => {
    if (!project || gitOperationRunning) return;
    const activeProject = project;
    const count = remoteOperationCommitCount(operation);
    gitOperationRunning = true;
    updateGitControls();
    renderRemoteOperation("running", operation, count);
    try {
      const result = await invoke<string>("git_remote_operation", {
        project: activeProject,
        operation,
        forceWithLease: false,
      });
      if (project !== activeProject) return;
      renderRemoteOperation("success", operation, count, result);
      await refreshGit(true);
      await refreshHistory(true);
    } catch (error) {
      if (project === activeProject) {
        renderRemoteOperation("error", operation, count, String(error));
      }
    } finally {
      gitOperationRunning = false;
      updateGitControls();
    }
  };
  fetchBtn.onclick = () => void remoteOperation("fetch");
  pullBtn.onclick = () => void remoteOperation("pull");
  pushBtn.onclick = () => void remoteOperation("push");
  const createCommit = async (push: boolean) => {
    const ok = await runGitMutation(t("workspace_commit"), "git_create_commit", {
      message: commitMessage.value,
      amend: amendInput.checked,
    });
    if (!ok) return;
    commitMessage.value = "";
    amendInput.checked = false;
    commitExpanded = false;
    setOperation(t("workspace_commit_success"));
    updateGitControls();
    await refreshHistory(true);
    if (push) remoteOperation("push");
  };
  commitBtn.onclick = () => void createCommit(false);
  commitPushBtn.onclick = () => void createCommit(true);
  commitLauncher.onclick = () => {
    commitExpanded = true;
    updateCommitPresentation();
    commitMessage.focus();
  };
  commitClose.onclick = () => {
    commitExpanded = false;
    commitMessage.blur();
    updateCommitPresentation();
  };
  commitMessage.oninput = updateGitControls;
  amendInput.onchange = updateGitControls;
  historyFilterBtn.onclick = () => void refreshHistory(true);
  historyQuery.onkeydown = (event) => {
    if (event.key === "Enter") void refreshHistory(true);
  };
  historyAuthor.onkeydown = (event) => {
    if (event.key === "Enter") void refreshHistory(true);
  };
  document.addEventListener("pointerdown", (event) => {
    if (commitMenu && !commitMenu.contains(event.target as Node)) closeCommitMenu();
  });
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
    const width = constrainedPanelWidth(window.innerWidth - event.clientX, mode);
    panel.style.width = `${width}px`;
    scheduleLayoutChange();
  });
  const finishResize = (event: PointerEvent) => {
    if (!resizing) return;
    resizing = false;
    document.body.classList.remove("ws-resizing");
    if (resizer.hasPointerCapture(event.pointerId)) resizer.releasePointerCapture(event.pointerId);
    localStorage.setItem(
      WIDTH_STORAGE_KEYS[widthStorageMode(mode)],
      String(panel.getBoundingClientRect().width),
    );
    scheduleLayoutChange();
  };
  resizer.addEventListener("pointerup", finishResize);
  resizer.addEventListener("pointercancel", finishResize);

  clearPreview();
  setPreviewCollapsed(previewCollapsed);
  renderState(fileTree, t("workspace_loading"));
  renderState(gitList, t("workspace_loading"));
  renderState(historyList, t("workspace_loading"));

  return {
    show(entryMode: WorkspaceEntryMode) {
      if (entryMode === null) {
        setOpened(false);
        return;
      }
      if (entryMode === "files" && mode !== "files") setMode("files");
      if (entryMode !== "files" && mode !== entryMode) setMode(entryMode);
      setOpened(true);
    },
    close() {
      setOpened(false);
      onClose();
    },
    setProject(nextProject) {
      if (project === nextProject) return;
      project = nextProject;
      projectRevision += 1;
      clearDirectoryObservers();
      lastGitFingerprint = null;
      gitRefreshSequence += 1;
      historyCommits = [];
      historyRequestSequence += 1;
      historyLoading = false;
      historyHasMore = true;
      stagedPaths = [];
      unstagedPaths = [];
      stagedCount = 0;
      commitExpanded = false;
      const labels = gitSyncButtonLabels(0, 0);
      pullBtn.textContent = labels.pull;
      pushBtn.textContent = labels.push;
      commitMessage.value = "";
      amendInput.checked = false;
      updateGitControls();
      setOperation("", false);
      closeCommitMenu();
      projectLabel.textContent = project ? projectBasename(project) : "";
      projectLabel.title = project ?? "";
      clearPreview();
      setPreviewCollapsed(mode === "shell" ? false : workspacePreviewStartsCollapsed(mode));
      renderState(fileTree, project ? t("workspace_loading") : "");
      renderState(gitList, project ? t("workspace_loading") : "");
      renderState(historyList, project ? t("workspace_loading") : "");
      gitMeta.textContent = "";
      if (!project) setOpened(false);
    },
    refresh: () => refresh(false),
  };
}
