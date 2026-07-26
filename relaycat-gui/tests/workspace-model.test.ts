import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  groupGitChanges,
  gitStatusFingerprint,
  gitHistoryPage,
  historyNearBottom,
  canRenderMarkdown,
  isMarkdownPreviewPath,
  MARKDOWN_RENDER_LIMIT,
  formatWorkspaceEntrySize,
  formatWorkspaceModifiedTime,
  previewLanguage,
  projectBasename,
  storedWorkspacePanelWidth,
  tokenizePreviewLine,
  tokenizeDiffLine,
  workspaceCommitPresentation,
  workspaceEntryDecoration,
  workspacePanelWidth,
} from "../src/workspace-model.ts";

const workspacePanelSource = readFileSync(
  new URL("../src/workspace-panel.ts", import.meta.url),
  "utf8",
);
const workspaceRustSource = readFileSync(
  new URL("../src-tauri/src/workspace.rs", import.meta.url),
  "utf8",
);

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

test("clamps and defaults the workspace panel width", () => {
  assert.equal(workspacePanelWidth(100), 280);
  assert.equal(workspacePanelWidth(420), 420);
  assert.equal(workspacePanelWidth(900), 720);
  assert.equal(workspacePanelWidth(Number.NaN), 360);
});

test("uses the default width when no saved preference exists", () => {
  assert.equal(storedWorkspacePanelWidth(null), 360);
  assert.equal(storedWorkspacePanelWidth(null, 440), 440);
  assert.equal(storedWorkspacePanelWidth("410"), 410);
});

test("extracts a project name from Unix and Windows paths", () => {
  assert.equal(projectBasename("/work/relaycat/"), "relaycat");
  assert.equal(projectBasename("C:\\work\\relaycat"), "relaycat");
});

test("detects common source languages from file names", () => {
  assert.equal(previewLanguage("src/main.ts"), "typescript");
  assert.equal(previewLanguage("src/lib.rs"), "rust");
  assert.equal(previewLanguage("config.yaml"), "yaml");
  assert.equal(previewLanguage("LICENSE"), null);
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

test("GUI renders Markdown up to the existing 512 KiB text limit", () => {
  assert.equal(MARKDOWN_RENDER_LIMIT, 512 * 1024);
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

test("tokenizes source lines into safe syntax-color segments", () => {
  assert.deepEqual(
    tokenizePreviewLine('const value = "x"; // note', "typescript"),
    [
      { text: "const", kind: "keyword" },
      { text: " value = ", kind: "plain" },
      { text: '"x"', kind: "string" },
      { text: "; ", kind: "plain" },
      { text: "// note", kind: "comment" },
    ],
  );
});

test("tokenizes Swift keywords strings and comments", () => {
  assert.deepEqual(
    tokenizePreviewLine('let title = "RelayCat" // note', "swift"),
    [
      { text: "let", kind: "keyword" },
      { text: " title = ", kind: "plain" },
      { text: '"RelayCat"', kind: "string" },
      { text: " ", kind: "plain" },
      { text: "// note", kind: "comment" },
    ],
  );
});

test("formats workspace entry metadata compactly", () => {
  assert.equal(formatWorkspaceEntrySize(0, true), "");
  assert.equal(formatWorkspaceEntrySize(999, false), "999 B");
  assert.equal(formatWorkspaceEntrySize(1536, false), "1.5 KB");
  assert.equal(formatWorkspaceEntrySize(2 * 1024 * 1024, false), "2 MB");
  assert.equal(formatWorkspaceModifiedTime(null), "");
  assert.match(formatWorkspaceModifiedTime(1_700_000_000), /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
});

test("commit presentation hides empty state and collapses behind a launcher", () => {
  assert.deepEqual(workspaceCommitPresentation(0, false), {
    showLauncher: false,
    showForm: false,
  });
  assert.deepEqual(workspaceCommitPresentation(2, false), {
    showLauncher: true,
    showForm: false,
  });
  assert.deepEqual(workspaceCommitPresentation(2, true), {
    showLauncher: false,
    showForm: true,
  });
});

test("diff syntax keeps prefixes while highlighting source tokens", () => {
  assert.deepEqual(tokenizeDiffLine('+let title = "RelayCat"', "App.swift"), {
    kind: "add",
    prefix: "+",
    tokens: [
      { text: "let", kind: "keyword" },
      { text: " title = ", kind: "plain" },
      { text: '"RelayCat"', kind: "string" },
    ],
  });
  assert.deepEqual(tokenizeDiffLine("@@ -1 +1 @@", "App.swift"), {
    kind: "hunk",
    prefix: "",
    tokens: [{ text: "@@ -1 +1 @@", kind: "plain" }],
  });
  assert.equal(tokenizeDiffLine("+++ b/App.swift", "App.swift").kind, "header");
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

test("folder rows use only the leading disclosure triangle", () => {
  assert.deepEqual(workspaceEntryDecoration(true), {
    disclosure: "›",
    icon: "",
  });
  assert.deepEqual(workspaceEntryDecoration(false), {
    disclosure: "",
    icon: "·",
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
