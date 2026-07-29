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

export function storedWorkspacePanelWidth(stored: string | null, fallback = 360): number {
  return workspacePanelWidth(stored === null ? fallback : Number(stored));
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
  | "yaml"
  | "swift"
  | "c"
  | "cpp"
  | "csharp"
  | "java"
  | "kotlin"
  | "php"
  | "ruby"
  | "objective-c"
  | "plain"
  | "diff";

export type PreviewTokenKind = "plain" | "keyword" | "string" | "number" | "comment";

export interface PreviewToken {
  text: string;
  kind: PreviewTokenKind;
}

export function previewLanguage(_path: string): PreviewLanguage | null {
  const fileName = _path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const special: Record<string, PreviewLanguage> = {
    dockerfile: "shell", containerfile: "shell", makefile: "shell", gnumakefile: "shell",
    "cmakelists.txt": "shell", jenkinsfile: "shell", procfile: "shell", podfile: "ruby",
    gemfile: "ruby", rakefile: "ruby", ".env": "plain", ".editorconfig": "plain",
    ".gitignore": "plain", ".dockerignore": "plain",
  };
  if (special[fileName]) return special[fileName];
  const extension = fileName.split(".").pop();
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
    jsonc: "json",
    json5: "json",
    jsonl: "json",
    ndjson: "json",
    map: "json",
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
    graphql: "plain",
    gql: "plain",
    proto: "plain",
    dart: "plain",
    lua: "plain",
    scala: "plain",
    vue: "html",
    svelte: "html",
    tf: "plain",
    tfvars: "plain",
    hcl: "plain",
    nix: "plain",
    sql: "plain",
    gradle: "plain",
    groovy: "plain",
    ini: "plain",
    conf: "plain",
    properties: "plain",
    env: "plain",
    plist: "plain",
    pem: "plain",
    crt: "plain",
    cer: "plain",
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
    swift: "//",
    c: "//",
    cpp: "//",
    csharp: "//",
    java: "//",
    kotlin: "//",
    php: "//",
    ruby: "#",
    "objective-c": "//",
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

export function workspaceCommitPresentation(
  stagedCount: number,
  expanded: boolean,
): { showLauncher: boolean; showForm: boolean } {
  const hasStagedChanges = stagedCount > 0;
  return {
    showLauncher: hasStagedChanges && !expanded,
    showForm: hasStagedChanges && expanded,
  };
}

export type DiffLineKind = "plain" | "add" | "del" | "hunk" | "header";

export interface DiffPreviewLine {
  kind: DiffLineKind;
  prefix: string;
  tokens: PreviewToken[];
}

export function tokenizeDiffLine(line: string, sourcePath: string): DiffPreviewLine {
  if (line.startsWith("@@")) {
    return { kind: "hunk", prefix: "", tokens: [{ text: line, kind: "plain" }] };
  }
  if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ")) {
    return { kind: "header", prefix: "", tokens: [{ text: line, kind: "plain" }] };
  }
  const prefix = /^[+\- ]/.test(line) ? line[0] : "";
  const source = prefix ? line.slice(1) : line;
  const language = previewLanguage(sourcePath);
  return {
    kind: prefix === "+" ? "add" : prefix === "-" ? "del" : "plain",
    prefix,
    tokens: language
      ? tokenizePreviewLine(source, language)
      : [{ text: source || (prefix ? "" : " "), kind: "plain" }],
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
  swift: [
    "actor", "as", "async", "await", "break", "case", "catch", "class", "continue",
    "default", "defer", "do", "else", "enum", "extension", "false", "for", "func",
    "guard", "if", "import", "in", "init", "let", "nil", "private", "protocol",
    "public", "return", "self", "static", "struct", "switch", "throw", "throws", "true",
    "try", "var", "where", "while",
  ],
  c: [
    "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "if", "int", "long", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void", "while",
  ],
  cpp: [
    "auto", "bool", "break", "case", "catch", "class", "const", "constexpr", "continue",
    "default", "delete", "else", "enum", "false", "for", "if", "namespace", "new", "nullptr",
    "private", "protected", "public", "return", "static", "struct", "switch", "template", "this",
    "throw", "true", "try", "using", "virtual", "void", "while",
  ],
  csharp: [
    "async", "await", "bool", "break", "case", "catch", "class", "const", "continue", "decimal",
    "default", "delegate", "else", "enum", "false", "for", "foreach", "if", "interface", "namespace",
    "new", "null", "private", "protected", "public", "return", "static", "string", "struct", "switch",
    "this", "throw", "true", "try", "using", "var", "virtual", "void", "while",
  ],
  java: [
    "abstract", "boolean", "break", "case", "catch", "class", "const", "continue", "default", "do",
    "else", "enum", "extends", "false", "final", "finally", "for", "if", "implements", "import",
    "instanceof", "interface", "new", "null", "package", "private", "protected", "public", "return",
    "static", "super", "switch", "this", "throw", "throws", "true", "try", "void", "while",
  ],
  kotlin: [
    "as", "break", "class", "continue", "data", "do", "else", "false", "for", "fun", "if", "import",
    "in", "interface", "is", "null", "object", "package", "private", "protected", "public", "return",
    "sealed", "super", "this", "throw", "true", "try", "typealias", "val", "var", "when", "while",
  ],
  php: [
    "abstract", "and", "array", "as", "break", "case", "catch", "class", "const", "continue", "default",
    "do", "echo", "else", "elseif", "extends", "false", "final", "finally", "for", "foreach", "function",
    "if", "implements", "interface", "namespace", "new", "null", "private", "protected", "public", "return",
    "static", "switch", "throw", "trait", "true", "try", "use", "while",
  ],
  ruby: [
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "for",
    "if", "in", "module", "next", "nil", "redo", "rescue", "retry", "return", "self", "super", "then",
    "true", "unless", "until", "when", "while", "yield",
  ],
  "objective-c": [
    "BOOL", "Class", "NO", "Nil", "SEL", "YES", "break", "case", "char", "const", "continue", "default",
    "do", "double", "else", "enum", "float", "for", "id", "if", "int", "long", "nil", "return", "self",
    "short", "static", "struct", "super", "switch", "typedef", "void", "while",
  ],
};

function compactDecimal(value: number): string {
  return value >= 10 || Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1);
}

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
