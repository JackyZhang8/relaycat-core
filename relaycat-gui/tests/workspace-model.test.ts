import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  groupGitChanges,
  gitStatusFingerprint,
  gitHistoryPage,
  historyNearBottom,
  previewLanguage,
  projectBasename,
  storedWorkspacePanelWidth,
  tokenizePreviewLine,
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
});
