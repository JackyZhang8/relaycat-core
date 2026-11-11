import { invoke } from "@tauri-apps/api/core";

import { purifySvgMarkup, renderSafeMarkdown } from "./markdown-preview";
import { applySyntaxRows, highlightMarkdownBlocks, highlightSyntax } from "./syntax-highlight";
import {
  developerPreviewKind,
  parseDelimitedPreview,
  sanitizeSvgPreview,
  type DeveloperPreviewKind,
} from "./developer-preview";

import {
  canCommit,
  buildSelectedLinesPatch,
  classifyGitError,
  diffStatLabel,
  gitOperationControls,
  gitMutationUsesInlineFeedback,
  gitOperationActions,
  gitSyncButtonLabels,
  historyFilterArgs,
  parseUnifiedDiff,
  type GitDiffStat,
  type GitErrorCode,
} from "./git-model";

import {
  groupChangesByDirectory,
  groupGitChanges,
  gitHistoryPage,
  gitStatusFingerprint,
  gitRelativeTime,
  clampImageZoom,
  imageFitScale,
  IMAGE_MAX_ZOOM,
  IMAGE_MIN_ZOOM,
  historyNearBottom,
  canRenderMarkdown,
  formatWorkspaceEntrySize,
  formatWorkspaceModifiedTime,
  isGitNotInstalledError,
  isMarkdownPreviewPath,
  previewLanguage,
  previewCollapseFor,
  projectBasename,
  storedWorkspacePanelWidth,
  parseDiffPreviewLine,
  uniqueWorkspaceEntries,
  workspacePanelWidth,
  NARROW_PANEL_WIDTH,
  WORKSPACE_PANEL_MIN_WIDTH,
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
interface MarkdownVisit {
  path: string;
  title: string;
  displayMode: "preview" | "source";
  scrollTop: number;
  scrollLeft: number;
}
interface MarkdownNavigationHistory {
  visits: MarkdownVisit[];
  index: number;
}
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

interface GitDiffStatDto {
  staged: Array<GitDiffStat & { path: string }>;
  unstaged: Array<GitDiffStat & { path: string }>;
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
  const openFolderBtn = document.querySelector("#workspace-open-folder") as HTMLButtonElement;
  const refreshProgress = document.querySelector("#workspace-refresh-progress") as HTMLElement;
  const projectLabel = document.querySelector("#workspace-project") as HTMLElement;
  const projectName = document.querySelector("#workspace-project-name") as HTMLElement;
  const workspaceContext = document.querySelector("#workspace-context") as HTMLElement;
  const gitTab = document.querySelector("#workspace-tab-git") as HTMLButtonElement;
  const historyTab = document.querySelector("#workspace-tab-history") as HTMLButtonElement;
  const filesView = document.querySelector("#workspace-files-view") as HTMLElement;
  const gitView = document.querySelector("#workspace-git-view") as HTMLElement;
  const historyView = document.querySelector("#workspace-history-view") as HTMLElement;
  const fileTree = document.querySelector("#workspace-file-tree") as HTMLElement;
  const fileSearch = document.querySelector("#workspace-file-search") as HTMLInputElement;
  const fileResults = document.querySelector("#workspace-file-results") as HTMLElement;
  const gitMeta = document.querySelector("#workspace-git-meta") as HTMLElement;
  const gitBanner = document.querySelector("#workspace-git-banner") as HTMLElement;
  const gitBannerText = document.querySelector("#workspace-git-banner-text") as HTMLElement;
  const gitBannerActions = document.querySelector("#workspace-git-banner-actions") as HTMLElement;
  const gitList = document.querySelector("#workspace-git-list") as HTMLElement;
  const gitFilter = document.querySelector("#workspace-git-filter") as HTMLElement;
  const gitFilterText = document.querySelector("#workspace-git-filter-text") as HTMLElement;
  const gitFilterClear = document.querySelector("#workspace-git-filter-clear") as HTMLButtonElement;
  const operationEl = document.querySelector("#workspace-operation") as HTMLElement;
  const branchSelect = document.querySelector("#workspace-branch") as HTMLSelectElement;
  const fetchBtn = document.querySelector("#workspace-fetch") as HTMLButtonElement;
  const pullBtn = document.querySelector("#workspace-pull") as HTMLButtonElement;
  const pushBtn = document.querySelector("#workspace-push") as HTMLButtonElement;
  const stageAllBtn = document.querySelector("#workspace-stage-all") as HTMLButtonElement;
  const unstageAllBtn = document.querySelector("#workspace-unstage-all") as HTMLButtonElement;
  const commitMessage = document.querySelector("#workspace-commit-message") as HTMLTextAreaElement;
  const commitStatus = document.querySelector("#workspace-commit-status") as HTMLElement;
  const commitHint = document.querySelector("#workspace-commit-hint") as HTMLElement;
  const commitRecentBtn = document.querySelector("#workspace-commit-recent") as HTMLButtonElement;
  const commitRecentList = document.querySelector("#workspace-commit-recent-list") as HTMLElement;
  const amendInput = document.querySelector("#workspace-amend") as HTMLInputElement;
  const commitBtn = document.querySelector("#workspace-commit") as HTMLButtonElement;
  const commitPushBtn = document.querySelector("#workspace-commit-push") as HTMLButtonElement;
  const historyList = document.querySelector("#workspace-history-list") as HTMLElement;
  const historyQuery = document.querySelector("#workspace-history-query") as HTMLInputElement;
  const historyAuthor = document.querySelector("#workspace-history-author") as HTMLInputElement;
  const historyRef = document.querySelector("#workspace-history-ref") as HTMLSelectElement;
  const historyFilterBtn = document.querySelector("#workspace-history-filter") as HTMLButtonElement;
  const historyFilterToggle = document.querySelector(
    "#workspace-history-filter-toggle",
  ) as HTMLButtonElement;
  const historyFiltersPanel = document.querySelector(
    "#workspace-history-filters",
  ) as HTMLElement;
  const historyChips = document.querySelector("#workspace-history-chips") as HTMLElement;
  const workspaceMain = document.querySelector(".ws-main") as HTMLElement;
  const previewResizer = document.querySelector("#workspace-preview-resizer") as HTMLElement;
  const previewTitle = document.querySelector("#workspace-preview-title") as HTMLButtonElement;
  const previewWrap = document.querySelector("#workspace-preview-wrap") as HTMLButtonElement;
  const previewActions = document.querySelector("#workspace-preview-actions") as HTMLElement;
  const previewNotice = document.querySelector("#workspace-preview-notice") as HTMLElement;
  const previewImage = document.querySelector("#workspace-preview-image") as HTMLImageElement;
  const imageView = document.querySelector("#workspace-image-view") as HTMLElement;
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
  let historyLoadError: string | null = null;
  const historyMore = document.createElement("button");
  historyMore.type = "button";
  historyMore.className = "ws-history-more";
  historyMore.setAttribute("aria-live", "polite");
  historyMore.onclick = () => void refreshHistory(false);
  let appliedHistoryFilters = historyFilterArgs("", "", "");
  let stagedCount = 0;
  // Unique changed paths in the repository, independent of the path filter, so
  // the header keeps reporting the real size of the change set.
  let changedPathCount = 0;
  let repositoryAhead = 0;
  let repositoryBehind = 0;
  let gitOperationRunning = false;
  let lastGitSections: {
    conflicted: GitDisplayChange[];
    staged: GitDisplayChange[];
    unstaged: GitDisplayChange[];
  } = { conflicted: [], staged: [], unstaged: [] };
  const collapsedGitDirs = new Set<string>();
  let commitMenu: HTMLElement | null = null;
  let fileMenu: HTMLElement | null = null;
  let copyNoticeTimer: number | null = null;
  const copyNotice = document.createElement("div");
  copyNotice.className = "ws-copy-notice";
  copyNotice.setAttribute("role", "status");
  copyNotice.setAttribute("aria-live", "polite");
  copyNotice.hidden = true;
  panel.appendChild(copyNotice);
  let gitPathFilter: { path: string; isDir: boolean } | null = null;
  let fileSearchSequence = 0;
  let fileSearchTimer: number | null = null;
  const RECENT_COMMIT_STORAGE_KEY = "relaycat.workspaceCommitHistory";
  const RECENT_COMMIT_LIMIT = 5;
  const COMMIT_SUBJECT_LIMIT = 50;
  let recentCommitMessages = loadRecentCommitMessages();
  let stagedPaths: string[] = [];
  let unstagedPaths: string[] = [];
  const currentPanelWidth = () => panel.getBoundingClientRect().width;
  let previewCollapsed = workspacePreviewStartsCollapsed();
  const PREVIEW_WRAP_KEY = "relaycat.workspacePreviewWrap";
  let previewWrapEnabled = localStorage.getItem(PREVIEW_WRAP_KEY) !== "0";
  // Image preview zoom. `imageZoom` is a scale where 1 means one image pixel
  // per CSS pixel, so the percentage readout is meaningful; "fit" is a mode,
  // not a separate value, so resizing the panel can recompute it.
  const IMAGE_ZOOM_STEP = 1.25;
  let imageNaturalWidth = 0;
  let imageNaturalHeight = 0;
  let imageZoom = 1;
  let imageFitMode = true;
  let imageZoomOutBtn: HTMLButtonElement | null = null;
  let imageZoomPctBtn: HTMLButtonElement | null = null;
  let imageZoomInBtn: HTMLButtonElement | null = null;
  let imageFitBtn: HTMLButtonElement | null = null;
  // Per-view UI state, so switching views keeps the user's place instead of
  // rebuilding the tree, the history list and the preview from scratch.
  const collapsedByMode: Partial<Record<WorkspaceMode, boolean>> = {};
  const previewRestore: Partial<Record<WorkspaceMode, () => void | Promise<void>>> = {};
  const loadedViews = new Set<WorkspaceMode>();
  let markdownDisplayMode: "preview" | "source" = "preview";
  let markdownSource: string | null = null;
  let markdownSourcePath = "";
  let markdownRenderedHtml = "";
  let markdownCanRender = false;
  let syntaxRenderSequence = 0;
  const markdownHistoryByMode: Partial<Record<WorkspaceMode, MarkdownNavigationHistory>> = {};
  let previewLoading = false;
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
      return Math.min(
        width,
        Math.max(WORKSPACE_PANEL_MIN_WIDTH, Math.floor(window.innerWidth * 0.6)),
      );
    }
    return width;
  }

  function syncNarrowClass(width: number) {
    // The branch select plus three sync buttons need more room than the panel
    // can give at its narrow end, so they collapse to icons/labels there.
    panel.classList.toggle("narrow", width > 0 && width < NARROW_PANEL_WIDTH);
  }

  function restorePanelWidth(value: WorkspaceMode) {
    const widthMode = widthStorageMode(value);
    const stored = localStorage.getItem(WIDTH_STORAGE_KEYS[widthMode]);
    const legacy = localStorage.getItem(LEGACY_WIDTH_STORAGE_KEY);
    const width = widthMode === "shell"
      ? storedWorkspacePanelWidth(stored, 440)
      : storedWorkspacePanelWidth(stored ?? legacy, 360);
    const constrained = constrainedPanelWidth(width, value);
    panel.style.width = `${constrained}px`;
    syncNarrowClass(constrained);
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

  function renderState(
    container: HTMLElement,
    message: string,
    options: { action?: { label: string; run: () => void }; hint?: string } = {},
  ) {
    container.innerHTML = "";
    const state = document.createElement("div");
    state.className = "ws-state";
    const text = document.createElement("div");
    text.textContent = message;
    state.appendChild(text);
    if (options.action) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ws-small-btn ws-state-action";
      button.textContent = options.action.label;
      button.onclick = () => options.action?.run();
      state.appendChild(button);
    }
    if (options.hint) {
      const hint = document.createElement("div");
      hint.className = "ws-state-hint";
      hint.textContent = options.hint;
      state.appendChild(hint);
    }
    container.appendChild(state);
  }

  function clearDirectoryObservers() {
    for (const observer of directoryObservers) observer.disconnect();
    directoryObservers.clear();
  }

  function applyPreviewWrap() {
    previewCode.classList.toggle("wrap", previewWrapEnabled);
    // The control only makes sense while code (not an image or notice) is on
    // screen, and it is pointless while the whole preview is collapsed.
    previewWrap.hidden = previewCode.hidden || previewCollapsed;
    previewWrap.setAttribute("aria-pressed", String(previewWrapEnabled));
  }

  // previewCode.hidden is toggled from several render paths (image, markdown,
  // notice, loading, error); observing the attribute keeps the wrap control in
  // sync from one place instead of touching every branch.
  new MutationObserver(applyPreviewWrap).observe(previewCode, {
    attributes: true,
    attributeFilter: ["hidden"],
  });

  previewImage.addEventListener("load", () => {
    imageNaturalWidth = previewImage.naturalWidth || 0;
    imageNaturalHeight = previewImage.naturalHeight || 0;
    if (!imageNaturalWidth || !imageNaturalHeight) {
      imageView.hidden = true;
      previewCode.hidden = false;
      previewCode.textContent = t("workspace_image_decode_failed");
      return;
    }
    fitImage();
  });
  previewImage.addEventListener("error", () => {
    if (imageView.hidden) return;
    imageView.hidden = true;
    previewCode.hidden = false;
    previewCode.textContent = t("workspace_image_decode_failed");
  });

  // Ctrl/Cmd + wheel is how desktop browsers report a pinch gesture; plain
  // wheel keeps scrolling (panning) the image.
  imageView.addEventListener(
    "wheel",
    (event) => {
      if (imageView.hidden || (!event.ctrlKey && !event.metaKey)) return;
      event.preventDefault();
      setImageZoom(imageZoom * Math.exp(-event.deltaY * 0.002), event.clientX, event.clientY);
    },
    { passive: false },
  );

  // WKWebView (the macOS shell) reports the trackpad pinch as Safari-style
  // gesture events instead of ctrl+wheel. preventDefault also stops the web
  // view from magnifying the whole page.
  let imageGestureBase = 1;
  imageView.addEventListener("gesturestart", (event: Event) => {
    if (imageView.hidden) return;
    event.preventDefault();
    imageGestureBase = imageZoom;
  });
  imageView.addEventListener("gesturechange", (event: Event) => {
    if (imageView.hidden) return;
    event.preventDefault();
    const scale = (event as Event & { scale?: number }).scale;
    if (typeof scale === "number" && Number.isFinite(scale)) {
      setImageZoom(imageGestureBase * scale);
    }
  });
  imageView.addEventListener("gestureend", (event: Event) => {
    if (imageView.hidden) return;
    event.preventDefault();
  });

  imageView.addEventListener("dblclick", () => {
    if (imageView.hidden) return;
    // Double-click toggles between "fit the window" and actual pixels.
    if (imageFitMode || imageZoom < 0.999) setImageZoom(1);
    else fitImage();
  });

  // Mouse users pan by dragging once the image overflows; trackpads can also
  // use the native two-finger scroll.
  let imagePan: { pointerId: number; x: number; y: number; left: number; top: number } | null =
    null;
  imageView.addEventListener("pointerdown", (event) => {
    if (imageView.hidden || event.button !== 0 || !imageCanPan()) return;
    imagePan = {
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      left: imageView.scrollLeft,
      top: imageView.scrollTop,
    };
    imageView.classList.add("panning");
    imageView.setPointerCapture(event.pointerId);
  });
  imageView.addEventListener("pointermove", (event) => {
    if (!imagePan || event.pointerId !== imagePan.pointerId) return;
    imageView.scrollLeft = imagePan.left - (event.clientX - imagePan.x);
    imageView.scrollTop = imagePan.top - (event.clientY - imagePan.y);
  });
  const finishImagePan = (event: PointerEvent) => {
    if (!imagePan || event.pointerId !== imagePan.pointerId) return;
    imagePan = null;
    imageView.classList.remove("panning");
    if (imageView.hasPointerCapture(event.pointerId)) {
      imageView.releasePointerCapture(event.pointerId);
    }
  };
  imageView.addEventListener("pointerup", finishImagePan);
  imageView.addEventListener("pointercancel", finishImagePan);
  imageView.addEventListener("keydown", (event) => {
    if (imageView.hidden) return;
    if (event.key === "+" || event.key === "=") {
      event.preventDefault();
      setImageZoom(imageZoom * IMAGE_ZOOM_STEP);
    } else if (event.key === "-" || event.key === "_") {
      event.preventDefault();
      setImageZoom(imageZoom / IMAGE_ZOOM_STEP);
    } else if (event.key === "0") {
      event.preventDefault();
      fitImage();
    } else if (event.key === "1") {
      event.preventDefault();
      setImageZoom(1);
    }
  });

  // Fit mode follows the available space when the panel or preview is resized.
  new ResizeObserver(() => {
    if (imageView.hidden || !imageFitMode || !imageNaturalWidth) return;
    if (imageView.clientWidth === 0) return;
    imageZoom = currentFitScale();
    applyImageSize();
  }).observe(imageView);

  function clearPreview() {
    previewRequestToken += 1;
    previewLoading = false;
    resetMarkdownPreview();
    resetDeveloperPreview();
    previewTitle.textContent = t("workspace_preview");
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    hideImagePreview();
    markdownPreview.hidden = true;
    markdownPreview.innerHTML = "";
    previewCode.hidden = false;
    previewCode.textContent = "";
    previewCode.classList.remove("show-line-numbers");
    previewActions.innerHTML = "";
  }

  function resetMarkdownPreview() {
    syntaxRenderSequence += 1;
    markdownDisplayMode = "preview";
    markdownSource = null;
    markdownSourcePath = "";
    markdownRenderedHtml = "";
    markdownCanRender = false;
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
    syntaxRenderSequence += 1;
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
        renderer = () => {
          const frame = document.createElement("div");
          frame.className = "ws-svg-preview";
          // The regex pre-sanitizer strips active content; DOMPurify is a
          // second pass against mutation-based bypasses before innerHTML.
          frame.innerHTML = purifySvgMarkup(safe);
          markdownPreview.appendChild(frame);
        };
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
    const navigation = document.createElement("div");
    navigation.className = "ws-markdown-navigation";
    const history = markdownHistoryByMode[mode];
    for (const direction of [-1, 1] as const) {
      const visit = history?.visits[history.index + direction];
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ws-small-btn ws-markdown-nav-btn";
      button.textContent = direction === -1 ? "<" : ">";
      const label = t(direction === -1 ? "workspace_markdown_back" : "workspace_markdown_forward");
      button.setAttribute("aria-label", label);
      button.title = visit ? `${label} · ${visit.path}` : label;
      button.disabled = previewLoading || !visit;
      button.onclick = () => navigateMarkdownHistory(direction);
      navigation.appendChild(button);
    }
    previewActions.appendChild(navigation);
  }

  function saveMarkdownVisit(targetMode: WorkspaceMode = mode) {
    const history = markdownHistoryByMode[targetMode];
    const visit = history?.visits[history.index];
    if (markdownSource === null || visit?.path !== markdownSourcePath) return;
    const viewport = markdownDisplayMode === "preview" ? markdownPreview : previewCode;
    visit.displayMode = markdownDisplayMode;
    visit.scrollTop = viewport.scrollTop;
    visit.scrollLeft = viewport.scrollLeft;
  }

  function recordMarkdownVisit(
    path: string,
    title: string,
    targetMode: WorkspaceMode,
    historyIndex?: number,
  ): MarkdownVisit {
    const history = markdownHistoryByMode[targetMode] ??= { visits: [], index: -1 };
    if (historyIndex !== undefined && history.visits[historyIndex]?.path === path) {
      history.index = historyIndex;
    } else if (history.visits[history.index]?.path !== path) {
      // A new navigation after Back replaces the old forward branch.
      history.visits.splice(history.index + 1);
      history.visits.push({ path, title, displayMode: "preview", scrollTop: 0, scrollLeft: 0 });
      history.index = history.visits.length - 1;
    }
    return history.visits[history.index];
  }

  function navigateMarkdownHistory(direction: -1 | 1) {
    if (previewLoading) return;
    const history = markdownHistoryByMode[mode];
    if (!history) return;
    const index = history.index + direction;
    const visit = history.visits[index];
    if (visit) void previewFile(visit.path, visit.title, mode, false, index);
  }

  function setMarkdownDisplayMode(value: "preview" | "source") {
    if (markdownSource === null) return;
    const sequence = ++syntaxRenderSequence;
    markdownDisplayMode = value;
    renderMarkdownActions(true);
    if (value === "preview") {
      previewCode.hidden = true;
      markdownPreview.innerHTML = markdownRenderedHtml;
      markdownPreview.hidden = false;
      void highlightMarkdownBlocks(markdownPreview, () => sequence === syntaxRenderSequence);
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
    applyPreviewWrap();
  }

  const PULL_REFRESH_THRESHOLD = 160;

  /** An explicit pull-down gesture at the top of a list refreshes that view.
   *  Switching views deliberately does not refetch, so this is the gesture
   *  that gives users control over freshness without interrupting reading. */
  function attachPullToRefresh(container: HTMLElement) {
    let pulled = 0;
    let lastWheelAt = 0;
    let cooldownUntil = 0;
    container.addEventListener(
      "wheel",
      (event) => {
        const now = performance.now();
        if (now < cooldownUntil) return;
        if (event.deltaY >= 0 || container.scrollTop > 0) {
          pulled = 0;
          return;
        }
        pulled = now - lastWheelAt > 300 ? -event.deltaY : pulled - event.deltaY;
        lastWheelAt = now;
        if (pulled < PULL_REFRESH_THRESHOLD) return;
        pulled = 0;
        cooldownUntil = now + 1200;
        void refresh();
      },
      { passive: true },
    );
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
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`;
    return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function renderHighlightedText(content: string, path: string) {
    const sequence = ++syntaxRenderSequence;
    previewCode.innerHTML = "";
    previewCode.classList.add("show-line-numbers");
    if (content === "") return;
    const fragment = document.createDocumentFragment();
    const rows: HTMLElement[] = [];
    for (const line of content.split("\n")) {
      const lineEl = document.createElement("span");
      lineEl.className = "ws-code-line";
      lineEl.textContent = line;
      rows.push(lineEl);
      fragment.appendChild(lineEl);
    }
    previewCode.appendChild(fragment);
    void highlightSyntax(content, previewLanguage(path)).then((lines) => {
      if (!lines || sequence !== syntaxRenderSequence) return;
      return applySyntaxRows(rows, lines, () => sequence === syntaxRenderSequence);
    });
  }

  function renderHighlightedDiff(content: string, sourcePath: string) {
    const sequence = ++syntaxRenderSequence;
    previewCode.innerHTML = "";
    previewCode.classList.remove("show-line-numbers");
    const fragment = document.createDocumentFragment();
    const rows: HTMLElement[] = [];
    let inHunk = false;
    for (const line of content.split("\n")) {
      if (line.startsWith("diff ")) inHunk = false;
      const rendered = parseDiffPreviewLine(line, inHunk);
      if (rendered.kind === "hunk") inHunk = true;
      const lineEl = document.createElement("span");
      lineEl.className = `ws-diff-line ${rendered.kind}`;
      if (rendered.prefix) {
        const prefix = document.createElement("span");
        prefix.className = "ws-diff-prefix";
        prefix.textContent = rendered.prefix;
        lineEl.appendChild(prefix);
      }
      const source = document.createElement("span");
      source.className = "ws-diff-source";
      source.textContent = rendered.source;
      lineEl.appendChild(source);
      rows.push(source);
      fragment.appendChild(lineEl);
    }
    previewCode.appendChild(fragment);
    void highlightSyntax(content, "diff", sourcePath).then((lines) => {
      if (!lines || sequence !== syntaxRenderSequence) return;
      // Keep row elements and their staging/selection handlers intact.
      return applySyntaxRows(rows, lines, () => sequence === syntaxRenderSequence);
    });
  }

  function hideImagePreview() {
    imageView.hidden = true;
    imageView.classList.remove("pannable", "panning");
    previewImage.removeAttribute("src");
    previewImage.style.width = "";
    previewImage.style.height = "";
    imageNaturalWidth = 0;
    imageNaturalHeight = 0;
    imageZoom = 1;
    imageFitMode = true;
  }

  /** Usable area inside the padded scroll container. */
  function imageViewportSize() {
    const style = getComputedStyle(imageView);
    const paddingX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
    const paddingY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
    return {
      width: Math.max(1, imageView.clientWidth - paddingX),
      height: Math.max(1, imageView.clientHeight - paddingY),
    };
  }

  /** Scale that shows the whole image in the current viewport. */
  function currentFitScale() {
    if (!imageNaturalWidth || !imageNaturalHeight) return 1;
    const { width, height } = imageViewportSize();
    return imageFitScale(imageNaturalWidth, imageNaturalHeight, width, height);
  }

  function updateImageActions() {
    if (!imageZoomPctBtn || !imageZoomOutBtn || !imageZoomInBtn || !imageFitBtn) return;
    imageZoomPctBtn.textContent = `${Math.round(imageZoom * 100)}%`;
    imageZoomPctBtn.title = imageNaturalWidth
      ? t("workspace_image_actual_size_hint", imageNaturalWidth, imageNaturalHeight)
      : t("workspace_image_actual_size");
    imageZoomOutBtn.disabled = imageZoom <= IMAGE_MIN_ZOOM + 0.001;
    imageZoomInBtn.disabled = imageZoom >= IMAGE_MAX_ZOOM - 0.001;
    imageFitBtn.classList.toggle("active", imageFitMode);
  }

  function applyImageSize() {
    if (!imageNaturalWidth) return;
    previewImage.style.width = `${Math.max(1, Math.round(imageNaturalWidth * imageZoom))}px`;
    previewImage.style.height = `${Math.max(1, Math.round(imageNaturalHeight * imageZoom))}px`;
    imageView.classList.toggle("pannable", imageCanPan());
    updateImageActions();
  }

  function imageCanPan() {
    return (
      imageView.scrollWidth > imageView.clientWidth + 1 ||
      imageView.scrollHeight > imageView.clientHeight + 1
    );
  }

  /**
   * Sets the zoom while keeping the image point under the anchor fixed, so
   * pinch/wheel zoom behaves like a map instead of jumping to the center.
   * Anchors are viewport client coordinates; omitted means image center.
   */
  function setImageZoom(next: number, anchorX?: number, anchorY?: number) {
    if (!imageNaturalWidth) return;
    const clamped = clampImageZoom(next);
    if (!imageFitMode && Math.abs(clamped - imageZoom) < 0.0001) return;
    const before = previewImage.getBoundingClientRect();
    const x = anchorX ?? before.left + before.width / 2;
    const y = anchorY ?? before.top + before.height / 2;
    const fractionX = before.width > 0 ? (x - before.left) / before.width : 0.5;
    const fractionY = before.height > 0 ? (y - before.top) / before.height : 0.5;
    imageZoom = clamped;
    imageFitMode = false;
    applyImageSize();
    const after = previewImage.getBoundingClientRect();
    imageView.scrollLeft += after.left + fractionX * after.width - x;
    imageView.scrollTop += after.top + fractionY * after.height - y;
  }

  function fitImage() {
    if (!imageNaturalWidth) return;
    imageFitMode = true;
    imageZoom = currentFitScale();
    imageView.scrollLeft = 0;
    imageView.scrollTop = 0;
    applyImageSize();
  }

  function renderImageActions() {
    previewActions.innerHTML = "";
    const group = document.createElement("div");
    group.className = "ws-image-zoom";
    const add = (label: string, title: string, onclick: () => void) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "ws-small-btn";
      button.textContent = label;
      button.title = title;
      button.onclick = onclick;
      group.appendChild(button);
      return button;
    };
    imageZoomOutBtn = add("−", t("workspace_image_zoom_out"), () =>
      setImageZoom(imageZoom / IMAGE_ZOOM_STEP),
    );
    imageZoomPctBtn = add("100%", t("workspace_image_actual_size"), () => setImageZoom(1));
    imageZoomPctBtn.classList.add("ws-image-zoom-pct");
    imageZoomInBtn = add("+", t("workspace_image_zoom_in"), () =>
      setImageZoom(imageZoom * IMAGE_ZOOM_STEP),
    );
    imageFitBtn = add(t("workspace_image_fit"), t("workspace_image_fit_hint"), () =>
      fitImage(),
    );
    previewActions.appendChild(group);
    updateImageActions();
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
    hideImagePreview();
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
      imageNaturalWidth = 0;
      imageNaturalHeight = 0;
      imageFitMode = true;
      imageZoom = 1;
      imageView.hidden = false;
      imageView.scrollLeft = 0;
      imageView.scrollTop = 0;
      renderImageActions();
      previewImage.src = preview.content;
      return;
    }
    if (!diff && preview.kind === "text" && isMarkdownPreviewPath(sourcePath)) {
      markdownSource = preview.content;
      markdownSourcePath = sourcePath;
      markdownCanRender = canRenderMarkdown(preview.size_bytes);
      if (markdownCanRender) {
        markdownRenderedHtml = renderSafeMarkdown(preview.content, sourcePath);
        setMarkdownDisplayMode("preview");
        markdownPreview.scrollTop = 0;
      } else {
        markdownDisplayMode = "source";
        renderMarkdownActions(false);
        previewNotice.textContent = t("workspace_markdown_too_large");
        previewNotice.classList.add("show");
        renderHighlightedText(preview.content, sourcePath);
      }
      return;
    }
    if (!preview.content) {
      previewCode.textContent = diff ? t("workspace_diff_empty") : "";
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
    saveMarkdownVisit();
    resetMarkdownPreview();
    resetDeveloperPreview();
    previewActions.innerHTML = "";
    setPreviewCollapsed(false);
    previewTitle.textContent = title;
    previewNotice.textContent = "";
    previewNotice.classList.remove("show");
    hideImagePreview();
    previewCode.hidden = false;
    previewCode.textContent = t("workspace_loading");
    if (isMarkdownPreviewPath(sourcePath)) renderMarkdownActions(false);
    else if (developerPreviewKind(sourcePath)) renderDeveloperActions(false);
  }

  function renderLoadError(container: HTMLElement, error: unknown) {
    renderState(container, t("workspace_load_failed", String(error)));
  }

  let previewRequestToken = 0;

  function activateMarkdownLink(event: MouseEvent | KeyboardEvent) {
    if (event instanceof KeyboardEvent && event.key !== "Enter") return;
    if (markdownSource === null) return;
    if (!(event.target instanceof Element)) return;
    const link = event.target.closest<HTMLElement>(
      ".ws-md-local-link[data-workspace-path]",
    );
    if (!link || !markdownPreview.contains(link)) return;
    event.preventDefault();
    event.stopPropagation();
    const path = link.dataset.workspacePath;
    if (path) void previewFile(path, path, mode);
  }
  markdownPreview.addEventListener("click", activateMarkdownLink);
  markdownPreview.addEventListener("keydown", activateMarkdownLink);

  function createEntryIcon(entry: WorkspaceEntry): HTMLElement {
    const icon = document.createElement("span");
    renderEntryIcon(icon, entry);
    return icon;
  }

  function renderEntryIcon(icon: HTMLElement, entry: WorkspaceEntry, expanded = false) {
    icon.className = `ws-tree-icon ${workspaceEntryDecoration(entry.is_dir).icon}`;
    icon.classList.toggle("expanded", expanded);
    icon.setAttribute("aria-hidden", "true");
    // Fixed-size outline icons keep files and folders in the same name column.
    const shape = entry.is_dir
      ? expanded
        ? '<path d="M20 10V6a2 2 0 0 0-2-2h-6l-2-2H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14l4-10H8l-4 10"/>'
        : '<path d="M3 7V5a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z"/><path d="M3 8h18"/>'
      : '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"/><path d="M14 2v6h6M8 13h8M8 17h6"/>';
    icon.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">${shape}</svg>`;
  }

  function selectFileRow(row: HTMLElement) {
    panel.querySelector(".ws-tree-row.selected, .ws-tree-result.selected")?.classList.remove("selected");
    row.classList.add("selected");
  }

  async function previewFile(
    relativePath: string,
    title = relativePath,
    targetMode: WorkspaceMode = "files",
    restore = false,
    historyIndex?: number,
  ) {
    if (!project) return;
    if (!restore) saveMarkdownVisit(targetMode);
    const requestToken = ++previewRequestToken;
    const activeProject = project;
    const revision = projectRevision;
    const keepMarkdown = !restore && mode === targetMode &&
      isMarkdownPreviewPath(relativePath) && markdownSource !== null;
    previewLoading = true;
    if (keepMarkdown) {
      // Keep the current document available if the target cannot be loaded.
      setPreviewCollapsed(false);
      renderMarkdownActions(false);
      previewNotice.textContent = t("workspace_loading");
      previewNotice.classList.add("show");
    } else if (!restore) {
      showPreviewLoading(title, relativePath);
    } else if (markdownSource !== null) {
      renderMarkdownActions(false);
    }
    try {
      const preview = await invoke<FilePreview>("read_workspace_file", {
        project: activeProject,
        relativePath,
        maxBytes: workspaceLocalPreviewLimit(relativePath),
      });
      if (
        requestToken !== previewRequestToken ||
        project !== activeProject ||
        projectRevision !== revision || mode !== targetMode
      )
        return;
      if (keepMarkdown && preview.kind !== "text") {
        throw new Error(preview.kind === "too_large"
          ? t("workspace_too_large", formatBytes(preview.size_bytes))
          : t("workspace_binary"));
      }
      if (keepMarkdown) saveMarkdownVisit(targetMode);
      const isMarkdown = isMarkdownPreviewPath(relativePath);
      const visit = isMarkdown && preview.kind === "text"
        ? recordMarkdownVisit(relativePath, title, targetMode, historyIndex)
        : undefined;
      previewLoading = false;
      previewRestore[targetMode] = () =>
        previewFile(relativePath, title, targetMode, true);
      renderPreview(title, preview, false, relativePath);
      if (visit && (restore || historyIndex !== undefined)) {
        if (canRenderMarkdown(preview.size_bytes) && markdownDisplayMode !== visit.displayMode) {
          setMarkdownDisplayMode(visit.displayMode);
        }
        const viewport = markdownDisplayMode === "preview" ? markdownPreview : previewCode;
        viewport.scrollTop = visit.scrollTop;
        viewport.scrollLeft = visit.scrollLeft;
      } else if (isMarkdown && markdownSource === null) {
        renderMarkdownActions(false);
      }
    } catch (error) {
      if (
        requestToken !== previewRequestToken ||
        project !== activeProject ||
        projectRevision !== revision || mode !== targetMode
      )
        return;
      previewLoading = false;
      if (keepMarkdown) {
        previewNotice.textContent = t("workspace_load_failed", String(error));
        previewNotice.classList.add("show");
        renderMarkdownActions(markdownCanRender);
      } else if (!restore) {
        previewCode.textContent = t("workspace_load_failed", String(error));
        if (isMarkdownPreviewPath(relativePath)) renderMarkdownActions(false);
      }
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
    const existingPaths = new Set<string>();
    if (append) {
      for (const child of Array.from(container.children)) {
        if (child instanceof HTMLElement && child.classList.contains("ws-tree-node")) {
          const path = child.dataset.workspacePath;
          if (path) existingPaths.add(path);
        }
      }
    }
    for (const entry of uniqueWorkspaceEntries(existingPaths, entries)) {
      const node = document.createElement("div");
      node.className = "ws-tree-node";
      node.dataset.workspacePath = entry.relative_path;
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-tree-row";
      row.style.paddingLeft = `${10 + depth * 18}px`;
      row.title = entry.relative_path;
      const arrow = document.createElement("span");
      arrow.className = "ws-tree-arrow";
      const decoration = workspaceEntryDecoration(entry.is_dir);
      arrow.setAttribute("aria-hidden", "true");
      if (decoration.disclosure) {
        arrow.innerHTML = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m6 4 4 4-4 4"/></svg>';
      }
      const icon = createEntryIcon(entry);
      const name = document.createElement("span");
      name.className = "ws-tree-name";
      name.textContent = entry.name;
      const metadata = document.createElement("span");
      metadata.className = "ws-tree-meta";
      const size = formatWorkspaceEntrySize(entry.size_bytes, entry.is_dir);
      const modified = formatWorkspaceModifiedTime(entry.modified_unix_seconds);
      metadata.textContent = [size, modified].filter(Boolean).join(" · ");
      row.append(arrow, icon, name);
      if (metadata.textContent) row.append(metadata);
      node.appendChild(row);

      if (entry.is_dir) {
        row.setAttribute("aria-expanded", "false");
        const children = document.createElement("div");
        children.className = "ws-tree-children";
        children.hidden = true;
        node.appendChild(children);
        let loaded = false;
        row.onclick = async () => {
          if (!children.hidden) {
            children.hidden = true;
            arrow.classList.remove("expanded");
            renderEntryIcon(icon, entry);
            row.setAttribute("aria-expanded", "false");
            return;
          }
          children.hidden = false;
          arrow.classList.add("expanded");
          renderEntryIcon(icon, entry, true);
          row.setAttribute("aria-expanded", "true");
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
        row.onclick = () => {
          selectFileRow(row);
          void previewFile(entry.relative_path);
        };
      }
      row.oncontextmenu = (event) => {
        event.preventDefault();
        event.stopPropagation();
        openFileMenu(entry, event.clientX, event.clientY);
      };
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
    const ok = await loadDirectoryPage("", fileTree, 0, activeProject, revision, 0, false);
    if (ok) loadedViews.add("files");
    return ok;
  }

  function renderSearchResults(entries: WorkspaceEntry[]) {
    fileResults.innerHTML = "";
    if (entries.length === 0) {
      renderState(fileResults, t("workspace_file_search_empty"));
      return;
    }
    for (const entry of entries) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-tree-result";
      row.title = entry.relative_path;
      row.appendChild(createEntryIcon(entry));
      const path = document.createElement("span");
      path.className = "ws-tree-result-path";
      path.textContent = entry.relative_path;
      row.appendChild(path);
      row.onclick = () => {
        selectFileRow(row);
        void previewFile(entry.relative_path);
      };
      row.oncontextmenu = (event) => {
        event.preventDefault();
        event.stopPropagation();
        openFileMenu(entry, event.clientX, event.clientY);
      };
      fileResults.appendChild(row);
    }
  }

  async function searchFiles(rawQuery: string) {
    const query = rawQuery.trim();
    if (!query) {
      fileSearchSequence += 1;
      fileResults.hidden = true;
      fileResults.innerHTML = "";
      fileTree.hidden = false;
      return;
    }
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    const sequence = ++fileSearchSequence;
    fileTree.hidden = true;
    fileResults.hidden = false;
    renderState(fileResults, t("workspace_loading"));
    try {
      const entries = await invoke<WorkspaceEntry[]>("search_workspace_files", {
        project: activeProject,
        query,
        limit: 100,
      });
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== fileSearchSequence
      ) return;
      renderSearchResults(entries.filter((entry) => !entry.is_dir));
    } catch (error) {
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== fileSearchSequence
      ) return;
      renderLoadError(fileResults, error);
    }
  }

  async function previewGitChange(change: GitDisplayChange, restore = false) {
    if (!project) return;
    previewRestore.git = () => previewGitChange(change, true);
    if (!change.staged && change.status === "?") {
      await previewFile(
        change.path,
        `${change.path} · ${t("workspace_untracked_preview")}`,
        "git",
        restore,
      );
      return;
    }
    const activeProject = project;
    const revision = projectRevision;
    if (!restore) showPreviewLoading(change.path);
    try {
      const preview = await invoke<FilePreview>("git_workspace_diff", {
        project: activeProject,
        relativePath: change.path,
        staged: change.staged,
      });
      if (project !== activeProject || projectRevision !== revision) return;
      renderPreview(change.path, preview, true);
      previewActions.innerHTML = "";
      // Seeing the diff is often the start of a question about the whole file
      // ("what else is in here?"), so offer the file preview next to the hunks.
      // Deleted paths have nothing left on disk to open.
      if (change.status !== "D") {
        const viewFile = document.createElement("button");
        viewFile.type = "button";
        viewFile.className = "ws-small-btn";
        viewFile.textContent = t("workspace_view_file");
        viewFile.onclick = () => void previewFile(change.path, change.path, "git");
        previewActions.appendChild(viewFile);
      }
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
          if (headerIndex < 0) return;
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

  function setOperation(message: string, show = true, detail = "") {
    operationEl.className = "ws-operation";
    operationEl.textContent = message;
    operationEl.classList.toggle("show", show && !!message);
    if (detail) operationEl.title = detail;
    else operationEl.removeAttribute("title");
  }

  const GIT_ERROR_KEYS: Record<GitErrorCode, string> = {
    auth_failed: "workspace_git_error_auth_failed",
    non_fast_forward: "workspace_git_error_non_fast_forward",
    no_upstream: "workspace_git_error_no_upstream",
    dirty_worktree: "workspace_git_error_dirty_worktree",
    conflict: "workspace_git_error_conflict",
    hook_failed: "workspace_git_error_hook_failed",
    nothing_to_commit: "workspace_git_error_nothing_to_commit",
    detached_head: "workspace_git_error_detached_head",
    lock: "workspace_git_error_lock",
  };

  /** Turns raw git stderr into an actionable sentence, keeping the original
   *  text available as the tooltip for anyone who needs the details. */
  function gitErrorMessage(error: unknown): { text: string; detail: string } {
    const detail = String(error);
    const code = classifyGitError(detail);
    return { text: code ? t(GIT_ERROR_KEYS[code]) : detail, detail };
  }

  function remoteOperationCommitCount(operation: "fetch" | "pull" | "push") {
    if (operation === "push") return repositoryAhead;
    if (operation === "pull") return repositoryBehind;
    return 0;
  }

  function remoteOperationLabel(operation: "fetch" | "pull" | "push", count: number) {
    if (operation === "fetch") return t("workspace_fetch");
    const labels = gitSyncButtonLabels(
      operation === "push" ? count : 0,
      operation === "pull" ? count : 0,
      t("workspace_pull"),
      t("workspace_push"),
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
    const hint = state === "error" ? gitErrorMessage(output) : null;
    const hintLine = hint && hint.text !== hint.detail ? `${hint.text}\n\n` : "";
    log.textContent = `${command}\n\n${hintLine}${normalizedGitOutput(output) || fallback}`;
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
    updateCommitBar();
  }

  function updateCommitBar() {
    commitStatus.textContent =
      stagedCount > 0
        ? t("workspace_commit_staged", stagedCount)
        : t("workspace_commit_empty");
    const firstLine = commitMessage.value.split("\n", 1)[0] ?? "";
    const subjectTooLong = firstLine.length > COMMIT_SUBJECT_LIMIT;
    commitHint.hidden = !subjectTooLong;
    if (subjectTooLong) {
      commitHint.textContent = t("workspace_commit_subject_long", firstLine.length);
    }
    commitMessage.placeholder =
      amendInput.checked && !commitMessage.value.trim()
        ? t("workspace_commit_amend_placeholder")
        : t("workspace_commit_placeholder");
  }

  function loadRecentCommitMessages(): string[] {
    try {
      const raw = localStorage.getItem(RECENT_COMMIT_STORAGE_KEY);
      if (!raw) return [];
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) return [];
      return parsed
        .filter(
          (value): value is string =>
            typeof value === "string" && value.trim().length > 0,
        )
        .slice(0, RECENT_COMMIT_LIMIT);
    } catch {
      return [];
    }
  }

  function renderRecentCommitMessages() {
    commitRecentBtn.disabled = recentCommitMessages.length === 0;
    commitRecentList.innerHTML = "";
    if (recentCommitMessages.length === 0) {
      commitRecentList.hidden = true;
      return;
    }
    for (const message of recentCommitMessages) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = message.split("\n", 1)[0];
      button.title = message;
      button.onclick = () => {
        commitMessage.value = message;
        commitRecentList.hidden = true;
        updateGitControls();
        commitMessage.focus();
      };
      commitRecentList.appendChild(button);
    }
  }

  function rememberCommitMessage(message: string) {
    const value = message.trim();
    if (!value) return;
    recentCommitMessages = [
      value,
      ...recentCommitMessages.filter((item) => item !== value),
    ].slice(0, RECENT_COMMIT_LIMIT);
    try {
      localStorage.setItem(RECENT_COMMIT_STORAGE_KEY, JSON.stringify(recentCommitMessages));
    } catch {
      // Storage can be unavailable; the in-memory list still works.
    }
    renderRecentCommitMessages();
  }

  async function runGitMutation(
    label: string,
    command: string,
    args: Record<string, unknown> = {},
  ): Promise<boolean> {
    if (!project || gitOperationRunning) return false;
    const activeProject = project;
    const revision = projectRevision;
    const inlineFeedback = gitMutationUsesInlineFeedback(command);
    gitOperationRunning = true;
    updateGitControls();
    if (!inlineFeedback) setOperation(t("workspace_operation_running", label));
    try {
      const result = await invoke<string | void>(command, { project: activeProject, ...args });
      if (project !== activeProject || projectRevision !== revision) return false;
      if (!inlineFeedback) {
        setOperation(typeof result === "string" && result ? result : label);
      }
      await refreshGit(true);
      return true;
    } catch (error) {
      if (project === activeProject && projectRevision === revision) {
        const message = gitErrorMessage(error);
        setOperation(message.text, true, message.detail);
      }
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
    if (change.conflicted) row.classList.add("conflicted");
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
    const stat = document.createElement("span");
    stat.className = "ws-git-stat";
    stat.textContent = diffStatLabel(change.stat);
    main.append(status, path, stat);
    main.onclick = () => void previewGitChange(change);
    const actions = document.createElement("span");
    actions.className = "ws-row-actions";
    if (change.conflicted) {
      // Staging a conflicted path is how Git records the resolution, so the
      // action says exactly that instead of the generic "stage".
      actions.appendChild(
        rowAction(t("workspace_mark_resolved"), "✓", false, () => {
          void runGitMutation(t("workspace_mark_resolved"), "git_stage_paths", {
            paths: [change.path],
          });
        }),
      );
    } else if (change.staged) {
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

  function gitSection(kind: "conflicted" | "staged" | "unstaged", title: string) {
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

  function buildGitDirRow(
    kind: "conflicted" | "staged" | "unstaged",
    group: { dir: string; changes: GitDisplayChange[]; stat: GitDiffStat },
  ) {
    const key = `${kind}:${group.dir}`;
    const collapsed = collapsedGitDirs.has(key);
    const row = document.createElement("button");
    row.type = "button";
    row.className = `ws-git-dir${collapsed ? " collapsed" : ""}`;
    row.dataset.gitDir = key;
    const chevron = document.createElement("span");
    chevron.className = "ws-git-dir-chevron";
    chevron.textContent = "▾";
    const path = document.createElement("span");
    path.className = "ws-git-dir-path";
    path.textContent = group.dir || t("workspace_git_root");
    path.title = group.dir || t("workspace_git_root");
    const count = document.createElement("span");
    count.className = "ws-git-dir-count";
    count.textContent = t("workspace_git_dir_count", group.changes.length);
    const stat = document.createElement("span");
    stat.className = "ws-git-stat";
    stat.textContent = diffStatLabel(group.stat);
    row.append(chevron, path, count, stat);
    row.onclick = () => {
      if (collapsedGitDirs.has(key)) collapsedGitDirs.delete(key);
      else collapsedGitDirs.add(key);
      renderGitSections();
    };
    return row;
  }

  function renderGitSections() {
    reconcileGitSections(
      lastGitSections.conflicted,
      lastGitSections.staged,
      lastGitSections.unstaged,
    );
  }

  function reconcileGitSections(
    conflicted: GitDisplayChange[],
    staged: GitDisplayChange[],
    unstaged: GitDisplayChange[],
  ) {
    lastGitSections = { conflicted, staged, unstaged };
    gitList.querySelector(".ws-state")?.remove();
    const existingRows = Array.from(
      gitList.querySelectorAll<HTMLElement>(".ws-git-row[data-change-key]"),
    );
    const existingByKey = new Map(
      existingRows.map((row) => [row.dataset.changeKey as string, row]),
    );

    const conflictedSection = gitSection("conflicted", t("workspace_conflicts"));
    const stagedSection = gitSection("staged", t("workspace_staged"));
    const unstagedSection = gitSection("unstaged", t("workspace_unstaged"));
    gitList.append(conflictedSection, stagedSection, unstagedSection);
    for (const [kind, section, items] of [
      ["conflicted", conflictedSection, conflicted],
      ["staged", stagedSection, staged],
      ["unstaged", unstagedSection, unstaged],
    ] as const) {
      const heading = section.querySelector(".ws-git-section-title") as HTMLElement;
      const rows = section.querySelector(".ws-git-section-rows") as HTMLElement;
      heading.textContent = `${heading.dataset.title} (${items.length})`;
      section.hidden = items.length === 0;
      const groups = groupChangesByDirectory(items);
      // A single group that only holds root files needs no directory header:
      // the section title already says everything there is to say.
      const showDirectories = groups.length > 1 || (groups[0]?.dir ?? "") !== "";
      const nodes: HTMLElement[] = [];
      for (const group of groups) {
        if (showDirectories) nodes.push(buildGitDirRow(kind, group));
        if (showDirectories && collapsedGitDirs.has(`${kind}:${group.dir}`)) continue;
        for (const item of group.changes) {
          const key = gitChangeKey(item);
          let row = existingByKey.get(key);
          // A path can turn from a normal change into a conflict (and back)
          // without changing its key, so the row is rebuilt when its shape no
          // longer matches.
          const staleShape = !!row && row.classList.contains("conflicted") !== item.conflicted;
          if (!row || staleShape) {
            if (row) row.remove();
            row = buildGitRow(item);
            row.classList.add("ws-row-enter");
            row.addEventListener("animationend", () => row?.classList.remove("ws-row-enter"), {
              once: true,
            });
          } else {
            const status = row.querySelector(".ws-git-status");
            if (status) status.textContent = item.status;
            const stat = row.querySelector(".ws-git-stat");
            if (stat) stat.textContent = diffStatLabel(item.stat);
            const main = row.querySelector(".ws-git-row-main") as HTMLButtonElement | null;
            if (main) main.onclick = () => void previewGitChange(item);
          }
          row.classList.toggle("in-group", showDirectories);
          nodes.push(row);
        }
      }
      rows.replaceChildren(...nodes);
    }
    renderContext();
  }

  const OPERATION_LABEL_KEYS: Record<string, string> = {
    rebase: "workspace_git_operation_rebase",
    merge: "workspace_git_operation_merge",
    cherry_pick: "workspace_git_operation_cherry_pick",
    revert: "workspace_git_operation_revert",
  };

  function operationActionLabel(action: "continue" | "skip" | "abort") {
    return action === "continue"
      ? t("workspace_git_continue")
      : action === "skip"
        ? t("workspace_git_skip")
        : t("workspace_git_abort");
  }

  async function runOperationAction(action: "continue" | "skip" | "abort") {
    const label = operationActionLabel(action);
    if (action === "abort") {
      const ok = await confirm(t("workspace_git_abort_confirm"));
      if (!ok) return;
    }
    const done = await runGitMutation(label, "git_operation_action", { action });
    if (done) await refreshHistory(true);
  }

  function renderGitBanner(operation: string | null) {
    gitBanner.hidden = !operation;
    gitBannerActions.innerHTML = "";
    if (!operation) {
      gitBannerText.textContent = "";
      return;
    }
    const name = t(OPERATION_LABEL_KEYS[operation] ?? "workspace_git_operation_in_progress");
    gitBannerText.textContent = `${name} · ${t("workspace_git_operation_hint")}`;
    for (const action of gitOperationActions(operation)) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = `ws-small-btn${action === "abort" ? " danger" : ""}`;
      button.textContent = operationActionLabel(action);
      button.onclick = () => void runOperationAction(action);
      gitBannerActions.appendChild(button);
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
      const [status, summary, refs, diffStat] = await Promise.all([
        invoke<GitStatus>("git_workspace_status", { project: activeProject }),
        invoke<RepositorySummary>("git_repository_summary", { project: activeProject }),
        invoke<GitRef[]>("git_list_refs", { project: activeProject }),
        invoke<GitDiffStatDto>("git_workspace_diff_stat", { project: activeProject }),
      ]);
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== gitRefreshSequence
      ) return true;
      loadedViews.add("git");
      const statSignature = [...diffStat.staged, ...diffStat.unstaged]
        .map((entry) => `${entry.path}:${entry.added}:${entry.deleted}:${entry.binary ? 1 : 0}`)
        .join("|");
      const fingerprint = JSON.stringify([
        gitStatusFingerprint(status),
        summary,
        refs,
        gitPathFilter,
        statSignature,
      ]);
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
        branchSelect.innerHTML = "";
        historyRef.innerHTML = `<option value="">${t("workspace_all_refs")}</option>`;
        const labels = gitSyncButtonLabels(0, 0, t("workspace_pull"), t("workspace_push"));
        pullBtn.textContent = labels.pull;
        pullBtn.title = labels.pull;
        pushBtn.textContent = labels.push;
        pushBtn.title = labels.push;
        renderGitBanner(null);
        renderState(gitList, t("workspace_not_repo"), {
          action: { label: t("workspace_init_repo"), run: () => void initRepository() },
        });
        lastGitSections = { conflicted: [], staged: [], unstaged: [] };
        changedPathCount = 0;
        renderContext();
        return true;
      }
      repositoryAhead = summary.ahead;
      repositoryBehind = summary.behind;
      renderGitBanner(summary.operation);
      gitMeta.textContent = `⎇ ${summary.branch ?? t("workspace_detached")}${summary.upstream ? ` · ${summary.upstream}` : ""} · ↑${summary.ahead} ↓${summary.behind}`;
      const labels = gitSyncButtonLabels(
        summary.ahead,
        summary.behind,
        t("workspace_pull"),
        t("workspace_push"),
      );
      pullBtn.textContent = labels.pull;
      pullBtn.title = labels.pull;
      pushBtn.textContent = labels.push;
      pushBtn.title = labels.push;
      branchSelect.innerHTML = "";
      historyRef.innerHTML = `<option value="">${t("workspace_all_refs")}</option>`;
      for (const kind of ["local", "remote"] as const) {
        const group = document.createElement("optgroup");
        group.label = kind === "local"
          ? t("workspace_ref_group_local")
          : t("workspace_ref_group_remote");
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
      const allSections = groupGitChanges(status.changes);
      // The path filter only narrows what the list shows: staging and the
      // commit bar still reflect the whole index, so filtered-out staged work
      // can never silently disable the commit button.
      const sections = {
        conflicted: allSections.conflicted.filter(matchesGitPathFilter),
        staged: allSections.staged.filter(matchesGitPathFilter),
        unstaged: allSections.unstaged.filter(matchesGitPathFilter),
      };
      const statByKey = new Map<string, GitDiffStat>();
      for (const entry of diffStat.staged) statByKey.set(`staged:${entry.path}`, entry);
      for (const entry of diffStat.unstaged) statByKey.set(`unstaged:${entry.path}`, entry);
      for (const change of [...allSections.staged, ...allSections.unstaged]) {
        change.stat = statByKey.get(gitChangeKey(change));
      }
      stagedPaths = [...new Set(allSections.staged.map((change) => change.path))];
      unstagedPaths = [...new Set(allSections.unstaged.map((change) => change.path))];
      stagedCount = stagedPaths.length;
      changedPathCount = new Set(status.changes.map((change) => change.path)).size;
      updateGitControls();
      if (
        sections.conflicted.length === 0 &&
        sections.staged.length === 0 &&
        sections.unstaged.length === 0
      ) {
        if (gitPathFilter) {
          renderState(gitList, t("workspace_filter_no_changes"), {
            action: {
              label: t("workspace_filter_clear_scope"),
              run: clearGitPathFilter,
            },
          });
        } else {
          renderState(gitList, t("workspace_no_changes"));
        }
        return true;
      }
      reconcileGitSections(sections.conflicted, sections.staged, sections.unstaged);
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
      if (isGitNotInstalledError(error)) {
        renderState(gitList, t("workspace_git_not_installed"), {
          hint: "git-scm.com/downloads",
        });
      } else {
        renderLoadError(gitList, error);
      }
      return false;
    }
  }

  async function previewCommit(commit: GitCommit, restore = false) {
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    const title = t("workspace_commit_diff", commit.short_hash);
    previewRestore.history = () => previewCommit(commit, true);
    if (!restore) showPreviewLoading(title);
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

  /**
   * Each reset mode loses a different amount of work, so the prompt names the
   * concrete consequences instead of just repeating the command.
   */
  function resetConfirmMessage(mode: "soft" | "mixed" | "hard", hash: string): string {
    if (mode === "soft") return t("workspace_reset_soft_confirm", hash);
    if (mode === "mixed") {
      return t("workspace_reset_mixed_confirm", hash, String(lastGitSections.staged.length));
    }
    return t("workspace_reset_hard_confirm", hash, String(lastGitSections.unstaged.length));
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
    add(t("workspace_tag"), false, () => {
      const name = window.prompt(t("workspace_create_tag_prompt"));
      if (name) {
        void runHistoryMutation(t("workspace_tag"), "git_create_tag", { name, target: commit.hash });
      }
    });
    add(t("workspace_cherry_pick"), false, () => {
      void runHistoryMutation(t("workspace_cherry_pick"), "git_commit_action", {
        action: "cherry_pick",
        commit: commit.hash,
      });
    });
    add(t("workspace_revert"), false, () => {
      // Revert adds an inverse commit; it does not rewrite history, so it is
      // not styled as a destructive action. The prompt still states what the
      // new commit will do.
      void confirm(t("workspace_revert_confirm", commit.short_hash)).then((ok) => {
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
        void confirm(resetConfirmMessage(mode, commit.short_hash)).then((ok) => {
          if (ok) {
            void runHistoryMutation(label, "git_reset_to", { commit: commit.hash, mode });
          }
        });
      });
    }
    document.body.appendChild(menu);
    commitMenu = menu;
  }

  function closeFileMenu() {
    fileMenu?.remove();
    fileMenu = null;
  }

  function hideCopyNotice() {
    if (copyNoticeTimer !== null) window.clearTimeout(copyNoticeTimer);
    copyNoticeTimer = null;
    copyNotice.hidden = true;
    copyNotice.textContent = "";
  }

  function showCopyNotice() {
    hideCopyNotice();
    copyNotice.hidden = false;
    copyNotice.textContent = t("workspace_path_copied");
    copyNoticeTimer = window.setTimeout(hideCopyNotice, 1500);
  }

  function matchesGitPathFilter(change: { path: string }): boolean {
    if (!gitPathFilter) return true;
    return gitPathFilter.isDir
      ? change.path.startsWith(`${gitPathFilter.path}/`)
      : change.path === gitPathFilter.path;
  }

  function openFileMenu(entry: WorkspaceEntry, x: number, y: number) {
    closeFileMenu();
    if (!project) return;
    const activeProject = project;
    const revision = projectRevision;
    const menu = document.createElement("div");
    menu.className = "ws-commit-menu";
    menu.style.left = `${Math.min(x, window.innerWidth - 200)}px`;
    menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - 100))}px`;
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = t(
      entry.is_dir ? "workspace_show_dir_changes" : "workspace_show_file_changes",
    );
    button.onclick = () => {
      closeFileMenu();
      gitPathFilter = { path: entry.relative_path, isDir: entry.is_dir };
      renderGitFilter();
      setMode("git");
      void refreshGit(true);
    };
    menu.appendChild(button);
    const copyPath = document.createElement("button");
    copyPath.type = "button";
    copyPath.textContent = t("workspace_copy_path");
    copyPath.onclick = async () => {
      copyPath.disabled = true;
      try {
        await invoke("copy_workspace_path", {
          project: activeProject,
          relativePath: entry.relative_path,
        });
        if (fileMenu === menu) closeFileMenu();
        if (project === activeProject && projectRevision === revision && opened) showCopyNotice();
      } catch (error) {
        copyPath.disabled = false;
        menu.querySelector(".ws-menu-error")?.remove();
        const message = document.createElement("div");
        message.className = "ws-menu-error";
        message.setAttribute("role", "alert");
        message.textContent = t("workspace_copy_path_failed", String(error));
        menu.appendChild(message);
      }
    };
    menu.appendChild(copyPath);
    document.body.appendChild(menu);
    fileMenu = menu;
  }

  function clearGitPathFilter() {
    gitPathFilter = null;
    renderGitFilter();
    void refreshGit(true);
  }

  function renderGitFilter() {
    gitFilter.hidden = !gitPathFilter;
    if (!gitPathFilter) {
      gitFilterText.textContent = "";
      return;
    }
    gitFilterText.textContent = t(
      gitPathFilter.isDir ? "workspace_filter_dir" : "workspace_filter_file",
      gitPathFilter.path,
    );
    gitFilterText.title = gitPathFilter.path;
  }

  const RELATIVE_TIME_KEYS: Record<string, string> = {
    minute: "workspace_time_minutes",
    hour: "workspace_time_hours",
    day: "workspace_time_days",
    month: "workspace_time_months",
    year: "workspace_time_years",
  };

  function historyDateLabel(date: string): string {
    const relative = gitRelativeTime(date);
    if (!relative) return date;
    if (relative.unit === "now") return t("workspace_time_now");
    return t(RELATIVE_TIME_KEYS[relative.unit], relative.value);
  }

  function renderHistoryChips() {
    const chips: Array<{ field: "query" | "author" | "reference"; text: string }> = [];
    if (appliedHistoryFilters.query) {
      chips.push({
        field: "query",
        text: t("workspace_filter_chip_query", appliedHistoryFilters.query),
      });
    }
    if (appliedHistoryFilters.author) {
      chips.push({
        field: "author",
        text: t("workspace_filter_chip_author", appliedHistoryFilters.author),
      });
    }
    if (appliedHistoryFilters.reference) {
      chips.push({
        field: "reference",
        text: t("workspace_filter_chip_ref", appliedHistoryFilters.reference),
      });
    }
    historyChips.replaceChildren(
      ...chips.map((chip) => {
        const element = document.createElement("span");
        element.className = "ws-filter-chip";
        const text = document.createElement("span");
        text.className = "ws-filter-chip-text";
        text.textContent = chip.text;
        text.title = chip.text;
        const clear = document.createElement("button");
        clear.type = "button";
        clear.className = "ws-filter-chip-clear";
        clear.textContent = "×";
        clear.title = t("workspace_filter_clear");
        clear.onclick = () => {
          if (chip.field === "query") historyQuery.value = "";
          else if (chip.field === "author") historyAuthor.value = "";
          else historyRef.value = "";
          void refreshHistory(true);
        };
        element.append(text, clear);
        return element;
      }),
    );
    historyFilterToggle.textContent =
      chips.length > 0 ? t("workspace_filter_active", chips.length) : t("workspace_filter");
    historyFilterToggle.classList.toggle("filtering", chips.length > 0);
  }

  function appendHistoryRows(commits: GitCommit[]) {
    for (const commit of commits) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ws-history-row";
      const subjectLine = commit.subject;
      const parsedDate = new Date(commit.date);
      const absoluteDate = Number.isNaN(parsedDate.getTime())
        ? commit.date
        : parsedDate.toLocaleString();
      row.title = `${subjectLine}\n${commit.author} · ${absoluteDate}`;
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
      // Relative time keeps the row narrow and stable across locales; the
      // absolute timestamp stays available in the tooltip.
      meta.textContent = `${commit.author} · ${historyDateLabel(commit.date)}`;
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

  function renderHistoryMore() {
    historyMore.remove();
    if (historyCommits.length === 0 || !historyHasMore) return;
    historyMore.disabled = historyLoading;
    historyMore.textContent = t(
      historyLoading ? "workspace_loading" : historyLoadError ? "workspace_history_retry" : "workspace_load_more",
    );
    historyMore.title = historyLoadError ?? "";
    historyList.appendChild(historyMore);
  }

  function loadHistoryNearBottom() {
    if (
      opened && mode === "history" && historyList.clientHeight > 0 &&
      historyCommits.length > 0 &&
      !historyLoading && historyHasMore && !historyLoadError &&
      historyNearBottom(historyList.scrollTop, historyList.clientHeight, historyList.scrollHeight)
    ) void refreshHistory(false);
  }

  async function refreshHistory(reset = true): Promise<boolean> {
    if (!project) return false;
    if (!reset && (historyLoading || !historyHasMore)) return true;
    const activeProject = project;
    const revision = projectRevision;
    const sequence = ++historyRequestSequence;
    historyLoading = true;
    historyLoadError = null;
    let loaded = false;
    if (reset) {
      appliedHistoryFilters = historyFilterArgs(
        historyQuery.value,
        historyAuthor.value,
        historyRef.value,
      );
      renderHistoryChips();
      historyCommits = [];
      historyHasMore = true;
      historyList.scrollTop = 0;
      renderState(historyList, t("workspace_loading"));
    }
    renderHistoryMore();
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
          loadedViews.add("history");
          renderState(historyList, t("workspace_not_repo"), {
            action: { label: t("workspace_init_repo"), run: () => void initRepository() },
          });
          renderContext();
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
      loadedViews.add("history");
      if (reset) historyList.innerHTML = "";
      historyMore.remove();
      historyCommits.push(...commits);
      historyHasMore = commits.length === page.limit;
      if (historyCommits.length === 0) {
        renderState(historyList, t("workspace_history_empty"));
      } else {
        appendHistoryRows(commits);
      }
      renderContext();
      loaded = true;
      return true;
    } catch (error) {
      if (
        project !== activeProject ||
        projectRevision !== revision ||
        sequence !== historyRequestSequence
      ) return true;
      historyLoadError = String(error);
      if (reset || historyCommits.length === 0) {
        renderState(historyList, t("workspace_load_failed", String(error)), {
          action: { label: t("workspace_history_retry"), run: () => void refreshHistory(true) },
        });
      }
      // Appending errors leave the existing rows intact; finally adds a retry footer.
      return false;
    } finally {
      if (sequence === historyRequestSequence) {
        historyLoading = false;
        renderHistoryMore();
        if (loaded && historyHasMore) requestAnimationFrame(loadHistoryNearBottom);
      }
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
    if (!next) saveMarkdownVisit();
    opened = next && !!project;
    if (!opened) hideCopyNotice();
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

  async function initRepository() {
    if (!project) return;
    const ok = await confirm(t("workspace_init_repo_confirm"));
    if (!ok) return;
    const created = await runGitMutation(t("workspace_init_repo"), "git_init_repository");
    if (created && mode === "history") void refreshHistory(true);
  }

  /**
   * Files mode hides the whole tab strip, so the header is the only place that
   * can say which section is on screen. The context hint also carries the size
   * of the list, which is the first thing you want to know after switching.
   */  function renderContext() {
    if (!project) {
      workspaceContext.textContent = "";
      return;
    }
    if (mode === "files") {
      workspaceContext.textContent = t("workspace_context_files");
      return;
    }
    if (mode === "shell") {
      workspaceContext.textContent = t("workspace_context_shell");
      return;
    }
    if (mode === "git") {
      workspaceContext.textContent =
        changedPathCount > 0
          ? t("workspace_context_changes", changedPathCount)
          : t("workspace_context_changes_empty");
      return;
    }
    const total = historyHasMore ? `${historyCommits.length}+` : String(historyCommits.length);
    workspaceContext.textContent =
      historyCommits.length > 0
        ? t("workspace_context_history", total)
        : t("workspace_context_history_empty");
  }

  function setMode(next: WorkspaceMode) {
    // Clicking the tab you are already on must not refetch or re-render.
    if (opened && mode === next) return;
    if (mode !== next) {
      saveMarkdownVisit();
      collapsedByMode[mode] = previewCollapsed;
      previewRequestToken += 1;
      previewLoading = false;
      syntaxRenderSequence += 1;
    }
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
    renderContext();
    if (shellActive) {
      stopRefreshTimer();
    } else {
      // Restore the preview this view had, so switching tabs does not throw
      // away what the user was reading. Only the first visit clears it.
      const restore = previewRestore[mode];
      if (restore) void restore();
      else clearPreview();
      setPreviewCollapsed(
        previewCollapseFor(collapsedByMode[mode], mode),
      );
      if (opened) {
        startRefreshTimer();
        // The DOM of an already loaded view is left untouched: stale views are
        // refreshed by the timer or the refresh button, not by every switch.
        if (!loadedViews.has(mode)) void refresh();
      }
    }
  }

  closeBtn.onclick = () => {
    setOpened(false);
    onClose();
  };

  // Tabs behave like a real tablist: left/right arrows move between them.
  const onTabKeydown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const next: WorkspaceMode = mode === "git" ? "history" : "git";
    setMode(next);
    (next === "git" ? gitTab : historyTab).focus();
  };
  gitTab.addEventListener("keydown", onTabKeydown);
  historyTab.addEventListener("keydown", onTabKeydown);

  panel.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    const target = event.target as HTMLElement | null;
    const tag = target?.tagName;
    // Escape inside a field should leave the field first, not the panel.
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      target?.blur();
      event.stopPropagation();
      return;
    }
    event.stopPropagation();
    setOpened(false);
    onClose();
  });
  refreshBtn.onclick = () => void refresh();
  openFolderBtn.onclick = async () => {
    if (!project || openFolderBtn.disabled) return;
    const activeProject = project;
    const revision = projectRevision;
    openFolderBtn.disabled = true;
    try {
      await invoke("open_workspace_folder", { project: activeProject });
    } catch (error) {
      if (project !== activeProject || projectRevision !== revision) return;
      previewNotice.textContent = t("workspace_open_folder_failed", String(error));
      previewNotice.classList.add("show");
      setPreviewCollapsed(false);
    } finally {
      if (project === activeProject && projectRevision === revision) openFolderBtn.disabled = false;
    }
  };
  for (const list of [fileTree, gitList, historyList]) attachPullToRefresh(list);
  fileSearch.oninput = () => {
    if (fileSearchTimer !== null) window.clearTimeout(fileSearchTimer);
    fileSearchTimer = window.setTimeout(() => {
      fileSearchTimer = null;
      void searchFiles(fileSearch.value);
    }, 180);
  };
  fileSearch.onkeydown = (event) => {
    if (event.key === "Escape") {
      fileSearch.value = "";
      void searchFiles("");
      return;
    }
    if (event.key === "Enter") {
      if (fileSearchTimer !== null) {
        window.clearTimeout(fileSearchTimer);
        fileSearchTimer = null;
      }
      void searchFiles(fileSearch.value).then(() => {
        fileResults.querySelector<HTMLButtonElement>(".ws-tree-result")?.click();
      });
    }
  };
  gitTab.onclick = () => setMode("git");
  historyTab.onclick = () => setMode("history");
  previewTitle.onclick = () => setPreviewCollapsed(!previewCollapsed);
  previewWrap.onclick = () => {
    previewWrapEnabled = !previewWrapEnabled;
    localStorage.setItem(PREVIEW_WRAP_KEY, previewWrapEnabled ? "1" : "0");
    applyPreviewWrap();
  };
  applyPreviewWrap();
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
    const message = commitMessage.value;
    const ok = await runGitMutation(t("workspace_commit"), "git_create_commit", {
      message,
      amend: amendInput.checked,
    });
    if (!ok) return;
    rememberCommitMessage(message);
    commitMessage.value = "";
    amendInput.checked = false;
    setOperation(t("workspace_commit_success"));
    updateGitControls();
    await refreshHistory(true);
    if (push) remoteOperation("push");
  };
  commitBtn.onclick = () => void createCommit(false);
  commitPushBtn.onclick = () => void createCommit(true);
  commitMessage.oninput = updateGitControls;
  commitMessage.onkeydown = (event) => {
    // Arrow-up on an empty composer recalls the last message, like a shell.
    if (
      event.key === "ArrowUp" &&
      !commitMessage.value.trim() &&
      recentCommitMessages.length > 0
    ) {
      event.preventDefault();
      commitMessage.value = recentCommitMessages[0];
      updateGitControls();
    }
  };
  commitRecentBtn.onclick = () => {
    if (recentCommitMessages.length === 0) return;
    commitRecentList.hidden = !commitRecentList.hidden;
  };
  gitFilterClear.onclick = () => clearGitPathFilter();
  amendInput.onchange = updateGitControls;
  const setHistoryFiltersOpen = (open: boolean) => {
    historyFiltersPanel.hidden = !open;
    historyFilterToggle.setAttribute("aria-expanded", String(open));
  };
  historyFilterToggle.onclick = () => {
    const open = historyFiltersPanel.hidden;
    setHistoryFiltersOpen(open);
    if (open) historyQuery.focus();
  };
  historyFiltersPanel.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      historyFilterBtn.click();
    } else if (event.key === "Escape") {
      event.preventDefault();
      setHistoryFiltersOpen(false);
      historyFilterToggle.focus();
    }
  });
  historyFilterBtn.onclick = () => {
    setHistoryFiltersOpen(false);
    void refreshHistory(true);
  };
  historyQuery.onkeydown = (event) => {
    if (event.key === "Enter") void refreshHistory(true);
  };
  historyAuthor.onkeydown = (event) => {
    if (event.key === "Enter") void refreshHistory(true);
  };
  document.addEventListener("pointerdown", (event) => {
    if (commitMenu && !commitMenu.contains(event.target as Node)) closeCommitMenu();
    const target = event.target as Node;
    if (fileMenu && !fileMenu.contains(target)) closeFileMenu();
    if (
      !commitRecentList.hidden &&
      !commitRecentList.contains(target) &&
      !commitRecentBtn.contains(target)
    ) {
      commitRecentList.hidden = true;
    }
  });
  historyList.addEventListener(
    "scroll",
    loadHistoryNearBottom,
    { passive: true },
  );
  new ResizeObserver(() => requestAnimationFrame(loadHistoryNearBottom)).observe(historyList);

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
    syncNarrowClass(width);
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
  resizer.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    if (window.matchMedia("(max-width: 760px)").matches) return;
    event.preventDefault();
    // The panel sits on the right edge, so moving the divider left widens it.
    const delta = event.key === "ArrowLeft" ? 16 : -16;
    const width = constrainedPanelWidth(currentPanelWidth() + delta, mode);
    panel.style.width = `${width}px`;
    syncNarrowClass(width);
    localStorage.setItem(WIDTH_STORAGE_KEYS[widthStorageMode(mode)], String(width));
    scheduleLayoutChange();
  });

  clearPreview();
  setPreviewCollapsed(previewCollapsed);
  renderRecentCommitMessages();
  updateCommitBar();
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
      hideCopyNotice();
      project = nextProject;
      projectRevision += 1;
      clearDirectoryObservers();
      lastGitFingerprint = null;
      gitRefreshSequence += 1;
      gitPathFilter = null;
      collapsedGitDirs.clear();
      lastGitSections = { conflicted: [], staged: [], unstaged: [] };
      historyQuery.value = "";
      historyAuthor.value = "";
      historyRef.value = "";
      appliedHistoryFilters = historyFilterArgs("", "", "");
      renderHistoryChips();
      renderGitFilter();
      fileSearch.value = "";
      fileSearchSequence += 1;
      fileResults.hidden = true;
      fileResults.innerHTML = "";
      fileTree.hidden = false;
      loadedViews.clear();
      for (const key of Object.keys(previewRestore)) {
        delete previewRestore[key as WorkspaceMode];
      }
      for (const key of Object.keys(markdownHistoryByMode)) {
        delete markdownHistoryByMode[key as WorkspaceMode];
      }
      for (const key of Object.keys(collapsedByMode)) {
        delete collapsedByMode[key as WorkspaceMode];
      }
      historyCommits = [];
      historyRequestSequence += 1;
      historyLoading = false;
      historyHasMore = true;
      historyLoadError = null;
      historyMore.remove();
      stagedPaths = [];
      unstagedPaths = [];
      stagedCount = 0;
      changedPathCount = 0;
      const labels = gitSyncButtonLabels(0, 0, t("workspace_pull"), t("workspace_push"));
      pullBtn.textContent = labels.pull;
      pullBtn.title = labels.pull;
      pushBtn.textContent = labels.push;
      pushBtn.title = labels.push;
      commitMessage.value = "";
      amendInput.checked = false;
      updateGitControls();
      setOperation("", false);
      closeCommitMenu();
      closeFileMenu();
      openFolderBtn.disabled = !project;
      projectName.textContent = project ? projectBasename(project) : "";
      projectLabel.title = project ?? "";
      renderContext();
      clearPreview();
      setPreviewCollapsed(
        mode === "shell"
          ? false
          : workspacePreviewStartsCollapsed(),
      );
      renderState(fileTree, project ? t("workspace_loading") : "");
      renderState(gitList, project ? t("workspace_loading") : "");
      renderState(historyList, project ? t("workspace_loading") : "");
      gitMeta.textContent = "";
      renderGitBanner(null);
      branchSelect.innerHTML = "";
      historyRef.innerHTML = `<option value="">${t("workspace_all_refs")}</option>`;
      renderHistoryChips();
      if (!project) setOpened(false);
    },
    refresh: () => refresh(false),
  };
}
