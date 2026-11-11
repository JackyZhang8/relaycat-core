export interface GitChange {
  path: string;
  index_status: string | null;
  worktree_status: string | null;
}

export interface WorkspaceEntry {
  name: string;
  relative_path: string;
  is_dir: boolean;
  size_bytes: number;
  modified_unix_seconds: number | null;
}

export interface WorkspaceEntriesPage {
  entries: WorkspaceEntry[];
  has_more: boolean;
  capped: boolean;
}

export function uniqueWorkspaceEntries<T extends Pick<WorkspaceEntry, "relative_path">>(
  existingPaths: Iterable<string>,
  entries: T[],
): T[] {
  const seen = new Set(existingPaths);
  return entries.filter((entry) => {
    if (seen.has(entry.relative_path)) return false;
    seen.add(entry.relative_path);
    return true;
  });
}

export interface FilePreview {
  kind: "text" | "image" | "archive" | "database" | "binary" | "too_large";
  content: string;
  mime_type: string | null;
  size_bytes: number;
}

export const LOCAL_PREVIEW_LIMITS = Object.freeze({
  text: 4 * 1024 * 1024,
  markdown: 8 * 1024 * 1024,
  image: 20 * 1024 * 1024,
  database: 512 * 1024 * 1024,
});

export const LOCAL_TEXT_PREVIEW_LIMIT = LOCAL_PREVIEW_LIMITS.text;
export const LOCAL_MARKDOWN_PREVIEW_LIMIT = LOCAL_PREVIEW_LIMITS.markdown;
export const LOCAL_IMAGE_PREVIEW_LIMIT = LOCAL_PREVIEW_LIMITS.image;
export const LOCAL_DATABASE_PREVIEW_LIMIT = LOCAL_PREVIEW_LIMITS.database;

export function workspaceLocalPreviewLimit(path: string): number {
  if (/\.(?:sqlite|sqlite3|db|db3)$/i.test(path)) return LOCAL_DATABASE_PREVIEW_LIMIT;
  if (/\.(?:png|jpe?g|gif|webp)$/i.test(path)) return LOCAL_IMAGE_PREVIEW_LIMIT;
  if (/\.(?:md|markdown)$/i.test(path)) return LOCAL_MARKDOWN_PREVIEW_LIMIT;
  return LOCAL_TEXT_PREVIEW_LIMIT;
}

export const MARKDOWN_RENDER_LIMIT = LOCAL_MARKDOWN_PREVIEW_LIMIT;

export function isMarkdownPreviewPath(path: string): boolean {
  return /\.(?:md|markdown)$/i.test(path);
}

export function canRenderMarkdown(byteCount: number): boolean {
  return Number.isFinite(byteCount) && byteCount >= 0 && byteCount <= MARKDOWN_RENDER_LIMIT;
}

export interface GitStatus {
  is_repo: boolean;
  branch: string | null;
  changes: GitChange[];
}

export function isGitNotInstalledError(error: unknown): boolean {
  return String(error).includes("git_not_installed");
}

export interface GitCommit {
  hash: string;
  short_hash: string;
  parents: string[];
  refs: string[];
  author: string;
  date: string;
  subject: string;
}

export interface GitDisplayChange {
  path: string;
  status: string;
  staged: boolean;
  conflicted: boolean;
  stat?: { added: number; deleted: number; binary: boolean };
}

// Porcelain v1 XY pairs that mean the path is in an unresolved conflict.
const CONFLICT_STATUSES = new Set(["DD", "AU", "UD", "UA", "DU", "AA", "UU"]);

export function isConflictedChange(
  change: Pick<GitChange, "index_status" | "worktree_status">,
): boolean {
  const status = `${change.index_status ?? " "}${change.worktree_status ?? " "}`;
  return CONFLICT_STATUSES.has(status);
}

/**
 * Every list view starts with the preview collapsed so the list gets the full
 * height; picking a file or change expands it (see showPreviewLoading). Shell
 * has no preview at all, which previewCollapseFor handles.
 */
export function workspacePreviewStartsCollapsed(): boolean {
  return true;
}

/**
 * Keeps the collapse choice a user made in a view when they switch back to it,
 * instead of re-applying the per-mode default on every switch.
 */
export function previewCollapseFor(
  remembered: boolean | undefined,
  mode: "files" | "git" | "history" | "shell",
): boolean {
  if (remembered !== undefined) return remembered;
  return mode === "shell" ? false : workspacePreviewStartsCollapsed();
}

export function groupGitChanges(changes: GitChange[]): {
  conflicted: GitDisplayChange[];
  staged: GitDisplayChange[];
  unstaged: GitDisplayChange[];
} {
  const conflicted: GitDisplayChange[] = [];
  const staged: GitDisplayChange[] = [];
  const unstaged: GitDisplayChange[] = [];
  for (const change of changes) {
    if (isConflictedChange(change)) {
      conflicted.push({
        path: change.path,
        status: `${change.index_status ?? " "}${change.worktree_status ?? " "}`.trim(),
        staged: false,
        conflicted: true,
      });
      continue;
    }
    if (change.index_status) {
      staged.push({
        path: change.path,
        status: change.index_status,
        staged: true,
        conflicted: false,
      });
    }
    if (change.worktree_status) {
      unstaged.push({
        path: change.path,
        status: change.worktree_status,
        staged: false,
        conflicted: false,
      });
    }
  }
  return { conflicted, staged, unstaged };
}

export interface GitChangeGroup {
  /** Directory of the changed files; "" means files directly in the root. */
  dir: string;
  changes: GitDisplayChange[];
  stat: { added: number; deleted: number; binary: boolean };
}

/**
 * Groups a section of the change list by the directory that holds each file,
 * so a large change set can be scanned directory by directory. Root files come
 * first, the remaining directories are ordered alphabetically.
 */
export function groupChangesByDirectory(changes: GitDisplayChange[]): GitChangeGroup[] {
  const buckets = new Map<string, GitDisplayChange[]>();
  for (const change of changes) {
    const slash = change.path.lastIndexOf("/");
    const dir = slash === -1 ? "" : change.path.slice(0, slash);
    const bucket = buckets.get(dir);
    if (bucket) bucket.push(change);
    else buckets.set(dir, [change]);
  }
  return [...buckets.entries()]
    .sort(([left], [right]) => {
      if (left === right) return 0;
      if (left === "") return -1;
      if (right === "") return 1;
      return left.localeCompare(right);
    })
    .map(([dir, items]) => {
      let added = 0;
      let deleted = 0;
      let binary = false;
      for (const item of items) {
        if (!item.stat) continue;
        binary = binary || item.stat.binary;
        added += item.stat.added;
        deleted += item.stat.deleted;
      }
      return { dir, changes: items, stat: { added, deleted, binary } };
    });
}

/** Image preview zoom bounds: 10% – 800%. */
export const IMAGE_MIN_ZOOM = 0.1;
export const IMAGE_MAX_ZOOM = 8;

export function clampImageZoom(zoom: number): number {
  if (!Number.isFinite(zoom)) return 1;
  return Math.min(IMAGE_MAX_ZOOM, Math.max(IMAGE_MIN_ZOOM, zoom));
}

/**
 * Scale that shows the whole image inside the viewport without ever enlarging
 * past 1:1 (small icons stay at their natural size instead of being blown up).
 */
export function imageFitScale(
  naturalWidth: number,
  naturalHeight: number,
  viewportWidth: number,
  viewportHeight: number,
): number {
  if (
    !(naturalWidth > 0) ||
    !(naturalHeight > 0) ||
    !(viewportWidth > 0) ||
    !(viewportHeight > 0)
  ) {
    return 1;
  }
  return Math.min(1, viewportWidth / naturalWidth, viewportHeight / naturalHeight);
}

export const WORKSPACE_PANEL_MIN_WIDTH = 320;export const WORKSPACE_PANEL_MAX_WIDTH = 720;
/** Below this width the Git toolbar switches to icon-only sync buttons. */
export const NARROW_PANEL_WIDTH = 380;

export function workspacePanelWidth(width: number): number {
  const finiteWidth = Number.isFinite(width) ? Math.round(width) : 360;
  return Math.max(
    WORKSPACE_PANEL_MIN_WIDTH,
    Math.min(WORKSPACE_PANEL_MAX_WIDTH, finiteWidth),
  );
}

export function storedWorkspacePanelWidth(stored: string | null, fallback = 360): number {
  return workspacePanelWidth(stored === null ? fallback : Number(stored));
}

export function projectBasename(project: string): string {
  const trimmed = project.replace(/[\\/]+$/, "");
  return trimmed.split(/[\\/]/).pop() || project;
}

export function previewLanguage(_path: string): string | null {
  const fileName = _path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const special: Record<string, string> = {
    dockerfile: "dockerfile", containerfile: "dockerfile", makefile: "makefile", gnumakefile: "makefile",
    "cmakelists.txt": "cmake", jenkinsfile: "groovy", procfile: "shell", podfile: "ruby",
    gemfile: "ruby", rakefile: "ruby", ".env": "dotenv", ".editorconfig": "ini",
    ".gitignore": "plain", ".dockerignore": "plain",
  };
  if (special[fileName]) return special[fileName];
  const extension = fileName.split(".").pop();
  if (!extension || extension === _path.toLowerCase()) return null;
  const languages: Record<string, string> = {
    ts: "typescript",
    tsx: "tsx",
    js: "javascript",
    jsx: "jsx",
    mjs: "javascript",
    cjs: "javascript",
    rs: "rust",
    py: "python",
    go: "go",
    sh: "shell",
    bash: "shell",
    zsh: "shell",
    json: "json",
    jsonc: "jsonc",
    json5: "json5",
    jsonl: "json",
    ndjson: "json",
    map: "json",
    md: "markdown",
    markdown: "markdown",
    html: "html",
    htm: "html",
    xml: "xml",
    svg: "xml",
    css: "css",
    scss: "scss",
    less: "less",
    toml: "toml",
    yaml: "yaml",
    yml: "yaml",
    swift: "swift",
    c: "c",
    h: "c",
    cc: "cpp",
    cpp: "cpp",
    cxx: "cpp",
    hpp: "cpp",
    cs: "csharp",
    java: "java",
    kt: "kotlin",
    kts: "kotlin",
    php: "php",
    rb: "ruby",
    m: "objective-c",
    mm: "objective-c",
    diff: "diff",
    patch: "diff",
    csv: "plain",
    tsv: "plain",
    graphql: "graphql",
    gql: "graphql",
    proto: "protobuf",
    dart: "dart",
    lua: "lua",
    scala: "scala",
    vue: "vue",
    svelte: "svelte",
    tf: "hcl",
    tfvars: "hcl",
    hcl: "hcl",
    nix: "nix",
    sql: "sql",
    gradle: "groovy",
    groovy: "groovy",
    ini: "ini",
    conf: "plain",
    properties: "properties",
    env: "dotenv",
    plist: "xml",
    pem: "plain",
    crt: "plain",
    cer: "plain",
  };
  return languages[extension] ?? null;
}

export function gitStatusFingerprint(status: GitStatus): string {
  return JSON.stringify([
    status.is_repo,
    status.branch,
    status.changes.map((change) => [
      change.path,
      change.index_status,
      change.worktree_status,
    ]),
  ]);
}

export function workspaceEntryDecoration(isDirectory: boolean): {
  disclosure: string;
  icon: string;
} {
  return isDirectory
    ? { disclosure: "›", icon: "folder" }
    : { disclosure: "", icon: "file" };
}

export function formatWorkspaceEntrySize(size: number, isDirectory: boolean): string {
  if (isDirectory || !Number.isFinite(size) || size < 0) return "";
  if (size < 1024) return `${Math.round(size)} B`;
  if (size < 1024 * 1024) return `${compactDecimal(size / 1024)} KB`;
  if (size < 1024 * 1024 * 1024) return `${compactDecimal(size / (1024 * 1024))} MB`;
  return `${compactDecimal(size / (1024 * 1024 * 1024))} GB`;
}

export function formatWorkspaceModifiedTime(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds < 0) {
    return "";
  }
  const date = new Date(seconds * 1000);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export type GitRelativeTimeUnit = "now" | "minute" | "hour" | "day" | "month" | "year";

/**
 * Coarse "how long ago" for history rows. Locale-independent: the caller maps
 * the unit to a translated string, so the panel keeps following the in-app
 * language rather than the OS locale.
 */
export function gitRelativeTime(
  date: string,
  now = Date.now(),
): { unit: GitRelativeTimeUnit; value: number } | null {
  const time = new Date(date).getTime();
  if (Number.isNaN(time) || !Number.isFinite(now)) return null;
  const diff = Math.max(0, now - time);
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  if (diff < minute) return { unit: "now", value: 0 };
  if (diff < hour) return { unit: "minute", value: Math.floor(diff / minute) };
  if (diff < day) return { unit: "hour", value: Math.floor(diff / hour) };
  if (diff < 30 * day) return { unit: "day", value: Math.floor(diff / day) };
  if (diff < 365 * day) return { unit: "month", value: Math.floor(diff / (30 * day)) };
  return { unit: "year", value: Math.floor(diff / (365 * day)) };
}

export type DiffLineKind = "plain" | "add" | "del" | "hunk" | "header";
export interface DiffPreviewLine {
  kind: DiffLineKind;
  prefix: string;
  source: string;
}

export function parseDiffPreviewLine(line: string, inHunk = false): DiffPreviewLine {
  if (line.startsWith("@@")) return { kind: "hunk", prefix: "", source: line };
  if (line.startsWith("diff ") || (!inHunk && /^(\+\+\+|---) /.test(line))) {
    return { kind: "header", prefix: "", source: line };
  }
  const prefix = inHunk && /^[+\- ]/.test(line) ? line[0] : "";
  return {
    kind: prefix === "+" ? "add" : prefix === "-" ? "del" : "plain",
    prefix,
    source: prefix ? line.slice(1) : line,
  };
}

export function gitHistoryPage(loaded: number): { skip: number; limit: number } {
  return { skip: Math.max(0, loaded), limit: 20 };
}

export function historyNearBottom(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= 100;
}

function compactDecimal(value: number): string {
  return value >= 10 || Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1);
}
