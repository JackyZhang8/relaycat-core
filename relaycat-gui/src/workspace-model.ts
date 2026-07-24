export interface GitChange {
  path: string;
  index_status: string | null;
  worktree_status: string | null;
}

export interface WorkspaceEntry {
  name: string;
  relative_path: string;
  is_dir: boolean;
}

export interface WorkspaceEntriesPage {
  entries: WorkspaceEntry[];
  has_more: boolean;
  capped: boolean;
}

export interface FilePreview {
  kind: "text" | "image" | "binary" | "too_large";
  content: string;
  mime_type: string | null;
  size_bytes: number;
}

export interface GitStatus {
  is_repo: boolean;
  branch: string | null;
  changes: GitChange[];
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
}

export function workspacePreviewStartsCollapsed(
  mode: "files" | "git" | "history",
): boolean {
  return mode === "git";
}

export function groupGitChanges(changes: GitChange[]): {
  staged: GitDisplayChange[];
  unstaged: GitDisplayChange[];
} {
  const staged: GitDisplayChange[] = [];
  const unstaged: GitDisplayChange[] = [];
  for (const change of changes) {
    if (change.index_status) {
      staged.push({
        path: change.path,
        status: change.index_status,
        staged: true,
      });
    }
    if (change.worktree_status) {
      unstaged.push({
        path: change.path,
        status: change.worktree_status,
        staged: false,
      });
    }
  }
  return { staged, unstaged };
}

export function workspacePanelWidth(width: number): number {
  const finiteWidth = Number.isFinite(width) ? Math.round(width) : 360;
  return Math.max(280, Math.min(720, finiteWidth));
}

export function storedWorkspacePanelWidth(stored: string | null): number {
  return workspacePanelWidth(stored === null ? Number.NaN : Number(stored));
}

export function projectBasename(project: string): string {
  const trimmed = project.replace(/[\\/]+$/, "");
  return trimmed.split(/[\\/]/).pop() || project;
}

export type PreviewLanguage =
  | "typescript"
  | "javascript"
  | "rust"
  | "python"
  | "go"
  | "shell"
  | "json"
  | "markdown"
  | "html"
  | "css"
  | "toml"
  | "yaml";

export type PreviewTokenKind = "plain" | "keyword" | "string" | "number" | "comment";

export interface PreviewToken {
  text: string;
  kind: PreviewTokenKind;
}

export function previewLanguage(_path: string): PreviewLanguage | null {
  const extension = _path.split(/[\\/]/).pop()?.split(".").pop()?.toLowerCase();
  if (!extension || extension === _path.toLowerCase()) return null;
  const languages: Record<string, PreviewLanguage> = {
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    rs: "rust",
    py: "python",
    go: "go",
    sh: "shell",
    bash: "shell",
    zsh: "shell",
    json: "json",
    md: "markdown",
    markdown: "markdown",
    html: "html",
    htm: "html",
    xml: "html",
    svg: "html",
    css: "css",
    scss: "css",
    less: "css",
    toml: "toml",
    yaml: "yaml",
    yml: "yaml",
  };
  return languages[extension] ?? null;
}

export function tokenizePreviewLine(
  line: string,
  language: PreviewLanguage,
): PreviewToken[] {
  if (language === "markdown" && /^\s*#{1,6}\s/.test(line)) {
    return [{ text: line, kind: "keyword" }];
  }
  const commentMarkers: Partial<Record<PreviewLanguage, string>> = {
    typescript: "//",
    javascript: "//",
    rust: "//",
    go: "//",
    python: "#",
    shell: "#",
    toml: "#",
    yaml: "#",
    html: "<!--",
    css: "/*",
  };
  const commentMarker = commentMarkers[language];
  const commentIndex = commentMarker ? commentStart(line, commentMarker) : -1;
  const code = commentIndex >= 0 ? line.slice(0, commentIndex) : line;
  const comment = commentIndex >= 0 ? line.slice(commentIndex) : "";
  const keywords = LANGUAGE_KEYWORDS[language] ?? [];
  const keywordPattern = keywords.length ? `|\\b(?:${keywords.join("|")})\\b` : "";
  const tokenPattern = new RegExp(
    `(?:"(?:\\\\.|[^"\\\\])*"|'(?:\\\\.|[^'\\\\])*'|\`(?:\\\\.|[^\`\\\\])*\`|\\b\\d+(?:\\.\\d+)?\\b${keywordPattern})`,
    "g",
  );
  const tokens: PreviewToken[] = [];
  let offset = 0;
  for (const match of code.matchAll(tokenPattern)) {
    const index = match.index ?? 0;
    if (index > offset) tokens.push({ text: code.slice(offset, index), kind: "plain" });
    const text = match[0];
    const kind: PreviewTokenKind = /^["'`]/.test(text)
      ? "string"
      : /^\d/.test(text)
        ? "number"
        : "keyword";
    tokens.push({ text, kind });
    offset = index + text.length;
  }
  if (offset < code.length) tokens.push({ text: code.slice(offset), kind: "plain" });
  if (comment) tokens.push({ text: comment, kind: "comment" });
  return tokens.length ? tokens : [{ text: line, kind: "plain" }];
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
    ? { disclosure: "›", icon: "" }
    : { disclosure: "", icon: "·" };
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

const LANGUAGE_KEYWORDS: Partial<Record<PreviewLanguage, string[]>> = {
  typescript: [
    "as",
    "async",
    "await",
    "class",
    "const",
    "else",
    "export",
    "extends",
    "false",
    "for",
    "from",
    "function",
    "if",
    "import",
    "interface",
    "let",
    "new",
    "null",
    "return",
    "true",
    "type",
    "undefined",
  ],
  javascript: [
    "async",
    "await",
    "class",
    "const",
    "else",
    "export",
    "extends",
    "false",
    "for",
    "from",
    "function",
    "if",
    "import",
    "let",
    "new",
    "null",
    "return",
    "true",
    "undefined",
  ],
  rust: [
    "async",
    "await",
    "const",
    "crate",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "let",
    "match",
    "mod",
    "mut",
    "pub",
    "return",
    "self",
    "struct",
    "trait",
    "true",
    "use",
    "where",
  ],
  python: [
    "and",
    "as",
    "async",
    "await",
    "class",
    "def",
    "elif",
    "else",
    "False",
    "for",
    "from",
    "if",
    "import",
    "in",
    "None",
    "not",
    "or",
    "return",
    "True",
    "while",
    "with",
  ],
  go: [
    "break",
    "case",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
  ],
  shell: ["case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "then", "while"],
  json: ["false", "null", "true"],
  html: ["html", "head", "body", "div", "span", "script", "style", "link", "meta"],
};

function commentStart(line: string, marker: string): number {
  let quote = "";
  let escaped = false;
  for (let index = 0; index <= line.length - marker.length; index += 1) {
    const char = line[index];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\" && quote) {
      escaped = true;
      continue;
    }
    if (quote) {
      if (char === quote) quote = "";
      continue;
    }
    if (char === '"' || char === "'" || char === "`") {
      quote = char;
      continue;
    }
    if (line.startsWith(marker, index)) return index;
  }
  return -1;
}
