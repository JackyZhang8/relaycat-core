import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  groupGitChanges,
  groupChangesByDirectory,
  gitRelativeTime,
  clampImageZoom,
  imageFitScale,
  isConflictedChange,
  isGitNotInstalledError,
  gitStatusFingerprint,
  gitHistoryPage,
  historyNearBottom,
  canRenderMarkdown,
  isMarkdownPreviewPath,
  LOCAL_PREVIEW_LIMITS,
  MARKDOWN_RENDER_LIMIT,
  formatWorkspaceEntrySize,
  formatWorkspaceModifiedTime,
  previewCollapseFor,
  previewLanguage,
  projectBasename,
  storedWorkspacePanelWidth,
  parseDiffPreviewLine,
  uniqueWorkspaceEntries,
  workspaceEntryDecoration,
  workspacePanelWidth,
  workspaceLocalPreviewLimit,
} from "../src/workspace-model.ts";

import { I18N } from "../src/i18n.ts";

const workspacePanelSource = readFileSync(
  new URL("../src/workspace-panel.ts", import.meta.url),
  "utf8",
);
const workspaceRustSource = readFileSync(
  new URL("../src-tauri/src/workspace.rs", import.meta.url),
  "utf8",
);

test("recognizes a missing Git installation and provides localized guidance", () => {
  assert.equal(isGitNotInstalledError("git_not_installed"), true);
  assert.equal(isGitNotInstalledError("加载失败：git_not_installed"), true);
  assert.equal(isGitNotInstalledError("not a git repository"), false);
  assert.equal(
    I18N.zh.workspace_git_not_installed,
    "未检测到 Git，请先安装 Git 后再使用工作区 Git 功能。",
  );
  assert.equal(
    I18N.en.workspace_git_not_installed,
    "Git was not detected. Install Git before using workspace Git features.",
  );
  assert.match(
    workspacePanelSource,
    /isGitNotInstalledError\(error\)[\s\S]{0,160}workspace_git_not_installed/,
  );
});

test("groups index and worktree changes into separate sections", () => {
  const result = groupGitChanges([
    { path: "src/a.ts", index_status: "M", worktree_status: null },
    { path: "src/b.ts", index_status: null, worktree_status: "M" },
    { path: "new.txt", index_status: null, worktree_status: "?" },
    { path: "both.ts", index_status: "A", worktree_status: "M" },
  ]);

  assert.deepEqual(
    result.staged.map(({ path, status }) => [path, status]),
    [
      ["src/a.ts", "M"],
      ["both.ts", "A"],
    ],
  );
  assert.deepEqual(
    result.unstaged.map(({ path, status }) => [path, status]),
    [
      ["src/b.ts", "M"],
      ["new.txt", "?"],
      ["both.ts", "M"],
    ],
  );
});

test("groups changes by directory with aggregated diff stats", () => {
  const groups = groupChangesByDirectory([
    { path: "src/a.ts", status: "M", staged: false, conflicted: false, stat: { added: 3, deleted: 1, binary: false } },
    { path: "src/ui/b.ts", status: "M", staged: false, conflicted: false, stat: { added: 2, deleted: 0, binary: false } },
    { path: "src/ui/c.png", status: "A", staged: false, conflicted: false, stat: { added: 0, deleted: 0, binary: true } },
    { path: "README.md", status: "M", staged: false, conflicted: false, stat: { added: 1, deleted: 1, binary: false } },
    { path: "no-stat.ts", status: "M", staged: false, conflicted: false },
  ]);

  assert.deepEqual(
    groups.map((group) => [group.dir, group.changes.map((change) => change.path)]),
    [
      ["", ["README.md", "no-stat.ts"]],
      ["src", ["src/a.ts"]],
      ["src/ui", ["src/ui/b.ts", "src/ui/c.png"]],
    ],
  );
  assert.deepEqual(groups[1].stat, { added: 3, deleted: 1, binary: false });
  assert.deepEqual(groups[2].stat, { added: 2, deleted: 0, binary: true });
  assert.deepEqual(groups[0].stat, { added: 1, deleted: 1, binary: false });
  assert.deepEqual(groupChangesByDirectory([]), []);
});

test("image zoom clamps and fit never enlarges small images", () => {
  assert.equal(clampImageZoom(0.01), 0.1);
  assert.equal(clampImageZoom(99), 8);
  assert.equal(clampImageZoom(Number.NaN), 1);
  assert.equal(imageFitScale(2000, 1000, 1000, 1000), 0.5);
  assert.equal(imageFitScale(2000, 1000, 500, 500), 0.25);
  assert.equal(imageFitScale(800, 600, 1000, 1000), 1);
  assert.equal(imageFitScale(0, 100, 500, 500), 1);
  assert.equal(imageFitScale(100, 100, 0, 500), 1);
});

test("history timestamps fall back to coarse relative units", () => {
  const now = Date.parse("2026-10-21T12:00:00Z");
  const at = (secondsAgo: number) => new Date(now - secondsAgo * 1000).toISOString();

  assert.deepEqual(gitRelativeTime(at(5), now), { unit: "now", value: 0 });
  assert.deepEqual(gitRelativeTime(at(3 * 60), now), { unit: "minute", value: 3 });
  assert.deepEqual(gitRelativeTime(at(5 * 3600), now), { unit: "hour", value: 5 });
  assert.deepEqual(gitRelativeTime(at(3 * 86400), now), { unit: "day", value: 3 });
  assert.deepEqual(gitRelativeTime(at(90 * 86400), now), { unit: "month", value: 3 });
  assert.deepEqual(gitRelativeTime(at(800 * 86400), now), { unit: "year", value: 2 });
  // Clock skew must not produce negative ages.
  assert.deepEqual(gitRelativeTime(at(-120), now), { unit: "now", value: 0 });
  assert.equal(gitRelativeTime("not a date", now), null);
});

test("conflicted paths are split into their own group", () => {
  const result = groupGitChanges([
    { path: "both.ts", index_status: "U", worktree_status: "U" },
    { path: "added.ts", index_status: "A", worktree_status: "A" },
    { path: "normal.ts", index_status: null, worktree_status: "M" },
  ]);

  assert.deepEqual(
    result.conflicted.map(({ path, status }) => [path, status]),
    [
      ["both.ts", "UU"],
      ["added.ts", "AA"],
    ],
  );
  assert.deepEqual(result.unstaged.map(({ path }) => path), ["normal.ts"]);
  assert.equal(result.staged.length, 0);
});

test("recognizes every porcelain conflict pair", () => {
  for (const code of ["DD", "AU", "UD", "UA", "DU", "AA", "UU"]) {
    assert.equal(
      isConflictedChange({ index_status: code[0], worktree_status: code[1] }),
      true,
    );
  }
  assert.equal(isConflictedChange({ index_status: "M", worktree_status: "M" }), false);
  assert.equal(isConflictedChange({ index_status: "A", worktree_status: null }), false);
});

test("clamps and defaults the workspace panel width", () => {
  assert.equal(workspacePanelWidth(100), 320);
  assert.equal(workspacePanelWidth(420), 420);
  assert.equal(workspacePanelWidth(900), 720);
  assert.equal(workspacePanelWidth(Number.NaN), 360);
});

test("uses the default width when no saved preference exists", () => {
  assert.equal(storedWorkspacePanelWidth(null), 360);
  assert.equal(storedWorkspacePanelWidth(null, 440), 440);
  assert.equal(storedWorkspacePanelWidth("410"), 410);
});

test("a remembered collapse choice wins over the per-view default", () => {
  // Every list view starts collapsed so the list gets the full height.
  assert.equal(previewCollapseFor(undefined, "git"), true);
  assert.equal(previewCollapseFor(undefined, "files"), true);
  assert.equal(previewCollapseFor(undefined, "history"), true);
  assert.equal(previewCollapseFor(undefined, "shell"), false);
  // Once the user opens the preview in a view, switching back keeps it open.
  assert.equal(previewCollapseFor(false, "git"), false);
  assert.equal(previewCollapseFor(false, "files"), false);
  assert.equal(previewCollapseFor(true, "files"), true);
});

test("extracts a project name from Unix and Windows paths", () => {
  assert.equal(projectBasename("/work/relaycat/"), "relaycat");
  assert.equal(projectBasename("C:\\work\\relaycat"), "relaycat");
});

test("uses wider local preview limits by development file category", () => {
  assert.equal(workspaceLocalPreviewLimit("README.md"), 8 * 1024 * 1024);
  assert.equal(workspaceLocalPreviewLimit("diagram.png"), 20 * 1024 * 1024);
  assert.equal(workspaceLocalPreviewLimit("cache.sqlite"), 512 * 1024 * 1024);
  assert.equal(workspaceLocalPreviewLimit("src/main.swift"), 4 * 1024 * 1024);
});

test("GUI preview limits match the Rust backend contract", () => {
  const rustLimit = (kind: "TEXT" | "MARKDOWN" | "IMAGE" | "DATABASE") => {
    const match = workspaceRustSource.match(
      new RegExp(`const MAX_LOCAL_${kind}_PREVIEW_BYTES: u64 = (\\d+) \\* 1024 \\* 1024;`),
    );
    assert.ok(match, `missing Rust ${kind.toLowerCase()} preview limit`);
    return Number(match[1]) * 1024 * 1024;
  };

  assert.deepEqual(LOCAL_PREVIEW_LIMITS, {
    text: rustLimit("TEXT"),
    markdown: rustLimit("MARKDOWN"),
    image: rustLimit("IMAGE"),
    database: rustLimit("DATABASE"),
  });
});

test("detects common source languages from file names", () => {
  assert.equal(previewLanguage("src/main.ts"), "typescript");
  assert.equal(previewLanguage("src/lib.rs"), "rust");
  assert.equal(previewLanguage("config.yaml"), "yaml");
  assert.equal(previewLanguage("LICENSE"), null);
});

test("detects special development filenames and formats", () => {
  assert.equal(previewLanguage("Dockerfile"), "dockerfile");
  assert.equal(previewLanguage("Makefile"), "makefile");
  assert.equal(previewLanguage("CMakeLists.txt"), "cmake");
  assert.equal(previewLanguage("schema.graphql"), "graphql");
  assert.equal(previewLanguage("change.patch"), "diff");
  assert.equal(previewLanguage("certificate.pem"), "plain");
});

test("detects mainstream native and JVM source languages", () => {
  assert.equal(previewLanguage("Sources/App.swift"), "swift");
  assert.equal(previewLanguage("native/main.c"), "c");
  assert.equal(previewLanguage("native/view.mm"), "objective-c");
  assert.equal(previewLanguage("native/engine.cpp"), "cpp");
  assert.equal(previewLanguage("App.cs"), "csharp");
  assert.equal(previewLanguage("Main.java"), "java");
  assert.equal(previewLanguage("Main.kt"), "kotlin");
  assert.equal(previewLanguage("index.php"), "php");
  assert.equal(previewLanguage("tool.rb"), "ruby");
});

test("detects Markdown preview files case-insensitively", () => {
  assert.equal(isMarkdownPreviewPath("README.md"), true);
  assert.equal(isMarkdownPreviewPath("docs/GUIDE.MARKDOWN"), true);
  assert.equal(isMarkdownPreviewPath("src/markdown.ts"), false);
});

test("GUI renders Markdown up to its wider local 8 MiB limit", () => {
  assert.equal(MARKDOWN_RENDER_LIMIT, 8 * 1024 * 1024);
  assert.equal(canRenderMarkdown(MARKDOWN_RENDER_LIMIT), true);
  assert.equal(canRenderMarkdown(MARKDOWN_RENDER_LIMIT + 1), false);
});

test("Markdown preview switches locally without requesting the file again", () => {
  assert.match(workspacePanelSource, /renderSafeMarkdown/);
  assert.match(workspacePanelSource, /workspace-markdown-preview/);
  assert.match(workspacePanelSource, /setMarkdownDisplayMode/);
  const setter = workspacePanelSource.match(
    /function setMarkdownDisplayMode[\s\S]*?\n  }/,
  )?.[0] ?? "";
  assert.doesNotMatch(setter, /invoke\s*</);
});

test("stale file previews resolve without a success value", () => {
  const previewFile = workspacePanelSource.match(
    /async function previewFile[\s\S]*?\n  }/,
  )?.[0] ?? "";
  assert.doesNotMatch(previewFile, /projectRevision !== revision\) return true;/);
});

test("developer preview switches locally while JSON stays in source mode", () => {
  assert.match(workspacePanelSource, /developerPreviewKind/);
  assert.doesNotMatch(workspacePanelSource, /parseJsonPreview/);
  assert.doesNotMatch(workspacePanelSource, /parseJsonLinesPreview/);
  assert.match(workspacePanelSource, /parseDelimitedPreview/);
  assert.match(workspacePanelSource, /sanitizeSvgPreview/);
  assert.match(workspacePanelSource, /purifySvgMarkup\(/);
  assert.match(workspacePanelSource, /setDeveloperDisplayMode/);
  const setter = workspacePanelSource.match(
    /function setDeveloperDisplayMode[\s\S]*?\n  }/,
  )?.[0] ?? "";
  assert.doesNotMatch(setter, /invoke\s*</);
});

test("formats workspace entry metadata compactly", () => {
  assert.equal(formatWorkspaceEntrySize(0, true), "");
  assert.equal(formatWorkspaceEntrySize(999, false), "999 B");
  assert.equal(formatWorkspaceEntrySize(1536, false), "1.5 KB");
  assert.equal(formatWorkspaceEntrySize(2 * 1024 * 1024, false), "2 MB");
  assert.equal(formatWorkspaceModifiedTime(null), "");
  assert.match(formatWorkspaceModifiedTime(1_700_000_000), /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
});

test("diff rows preserve source text and distinguish headers from hunk content", () => {
  assert.deepEqual(parseDiffPreviewLine('+let title = "RelayCat"', true), {
    kind: "add", prefix: "+", source: 'let title = "RelayCat"',
  });
  assert.deepEqual(parseDiffPreviewLine("@@ -1 +1 @@"), {
    kind: "hunk", prefix: "", source: "@@ -1 +1 @@",
  });
  assert.equal(parseDiffPreviewLine("+++ b/App.swift").kind, "header");
  assert.deepEqual(parseDiffPreviewLine("+++ b/App.swift", true), {
    kind: "add", prefix: "+", source: "++ b/App.swift",
  });
});

test("git status fingerprint changes only when repository state changes", () => {
  const status = {
    is_repo: true,
    branch: "main",
    changes: [{ path: "src/main.ts", index_status: null, worktree_status: "M" }],
  };
  assert.equal(gitStatusFingerprint(status), gitStatusFingerprint(structuredClone(status)));
  assert.notEqual(
    gitStatusFingerprint(status),
    gitStatusFingerprint({ ...status, branch: "feature" }),
  );
});

test("folder and file rows have distinct icons with folder-only disclosure", () => {
  assert.deepEqual(workspaceEntryDecoration(true), {
    disclosure: "›",
    icon: "folder",
  });
  assert.deepEqual(workspaceEntryDecoration(false), {
    disclosure: "",
    icon: "file",
  });
});

test("git history loads twenty commits at a time", () => {
  assert.deepEqual(gitHistoryPage(0), { skip: 0, limit: 20 });
  assert.deepEqual(gitHistoryPage(20), { skip: 20, limit: 20 });
});

test("git history loads the next page only near the bottom", () => {
  assert.equal(historyNearBottom(700, 220, 1000), true);
  assert.equal(historyNearBottom(500, 220, 1000), false);
});

test("workspace directories load one hundred entries per scroll page", () => {
  assert.match(workspaceRustSource, /const MAX_DIRECTORY_ENTRIES:\s*usize = 1000;/);
  assert.match(workspaceRustSource, /const DIRECTORY_PAGE_SIZE:\s*usize = 100;/);
  assert.match(workspaceRustSource, /struct WorkspaceEntriesPageDto/);
  assert.match(
    workspacePanelSource,
    /loadDirectoryPage\("",\s*fileTree,\s*0,\s*activeProject,\s*revision,\s*0,\s*false\)/s,
  );
  assert.match(workspacePanelSource, /offset,\s*limit:\s*100/s);
  assert.match(workspacePanelSource, /new IntersectionObserver/);
  assert.match(workspacePanelSource, /page\.has_more/);
  assert.match(workspacePanelSource, /page\.capped/);
  assert.match(workspaceRustSource, /size_bytes:\s*entry\.size/);
  assert.match(
    workspaceRustSource,
    /modified_unix_seconds:\s*entry\.modified_unix_seconds/,
  );
});

test("appended workspace directory pages skip paths already rendered", () => {
  assert.deepEqual(
    uniqueWorkspaceEntries(new Set(["src/a.ts"]), [
      { relative_path: "src/a.ts", name: "a.ts" },
      { relative_path: "src/b.ts", name: "b.ts" },
      { relative_path: "src/b.ts", name: "b duplicate.ts" },
      { relative_path: "src/c.ts", name: "c.ts" },
    ]),
    [
      { relative_path: "src/b.ts", name: "b.ts" },
      { relative_path: "src/c.ts", name: "c.ts" },
    ],
  );
});
