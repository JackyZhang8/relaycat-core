import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import {
  canCommit,
  buildSelectedLinesPatch,
  changeActionPolicy,
  gitOperationControls,
  gitMutationUsesInlineFeedback,
  gitSyncButtonLabels,
  reconcileGitChangeKeys,
  historyFilterArgs,
  parseUnifiedDiff,
} from "../src/git-model.ts";

const workspacePanel = fs.readFileSync(new URL("../src/workspace-panel.ts", import.meta.url), "utf8");
const html = fs.readFileSync(new URL("../index.html", import.meta.url), "utf8");
const gitRunner = fs.readFileSync(
  new URL("../src-tauri/src/git/runner.rs", import.meta.url),
  "utf8",
);

test("discard requires confirmation while staging stays immediate", () => {
  const policy = changeActionPolicy({ conflict: false });
  assert.equal(policy.stage.requiresConfirm, false);
  assert.equal(policy.discard.requiresConfirm, true);
});

test("unified diff is split into selectable hunks", () => {
  const files = parseUnifiedDiff(
    "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
  );
  assert.equal(files.length, 1);
  assert.equal(files[0].path, "a.txt");
  assert.equal(files[0].hunks.length, 1);
  assert.match(files[0].hunks[0].patch, /@@ -1 \+1 @@/);
});

test("selected-line patch turns unselected deletions into context", () => {
  const patch = buildSelectedLinesPatch(
    "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt",
    "@@ -1,2 +1,2 @@",
    ["-old one", "+new one", "-old two", "+new two"],
    new Set([0, 1]),
  );
  assert.match(patch, /@@ -1,2 \+1,2 @@/);
  assert.match(patch, /-old one\n\+new one/);
  assert.match(patch, / old two/);
  assert.doesNotMatch(patch, /\+new two/);
});

test("commit requires a message and staged changes unless amending", () => {
  assert.equal(canCommit("修复分页", 2, false), true);
  assert.equal(canCommit("   ", 2, false), false);
  assert.equal(canCommit("更新提交", 0, false), false);
  assert.equal(canCommit("更新提交", 0, true), true);
});

test("remote controls are locked while an operation is running", () => {
  assert.deepEqual(gitOperationControls(true), {
    fetchDisabled: true,
    pullDisabled: true,
    pushDisabled: true,
    branchDisabled: true,
  });
});

test("history filters trim empty values and preserve selected refs", () => {
  assert.deepEqual(historyFilterArgs(" fix ", " Alice ", "main"), {
    query: "fix",
    author: "Alice",
    reference: "main",
  });
  assert.deepEqual(historyFilterArgs(" ", "", ""), {
    query: null,
    author: null,
    reference: null,
  });
});

test("staging reconciles only the changed row instead of replacing the list", () => {
  assert.deepEqual(
    reconcileGitChangeKeys(
      ["staged:a.ts", "unstaged:b.ts"],
      ["staged:a.ts", "staged:b.ts"],
    ),
    {
      retained: ["staged:a.ts"],
      added: ["staged:b.ts"],
      removed: ["unstaged:b.ts"],
    },
  );
});

test("stage operations keep feedback inside the row instead of shifting the list", () => {
  assert.equal(gitMutationUsesInlineFeedback("git_stage_paths"), true);
  assert.equal(gitMutationUsesInlineFeedback("git_unstage_paths"), true);
  assert.equal(gitMutationUsesInlineFeedback("git_apply_patch"), true);
  assert.equal(gitMutationUsesInlineFeedback("git_remote_operation"), false);
});

test("pull and push buttons show pending commit counts", () => {
  assert.deepEqual(gitSyncButtonLabels(3, 2), {
    pull: "Pull(2)",
    push: "Push(3)",
  });
  assert.deepEqual(gitSyncButtonLabels(0, 0), {
    pull: "Pull",
    push: "Push",
  });
});

test("remote operations render running progress and a clear completion state", () => {
  assert.match(html, /id="workspace-operation"[^>]*aria-live="polite"/);
  assert.match(workspacePanel, /renderRemoteOperation\(\s*"running"/s);
  assert.match(workspacePanel, /renderRemoteOperation\(\s*"success"/s);
  assert.match(workspacePanel, /renderRemoteOperation\(\s*"error"/s);
  assert.match(workspacePanel, /remoteOperationCommitCount\(operation/);
});

test("history mutations report globally and refresh history after success", () => {
  assert.match(
    html,
    /id="workspace-operation"[\s\S]*?<div class="ws-main">/,
  );
  assert.match(workspacePanel, /async function runHistoryMutation\(/);
  assert.match(workspacePanel, /if \(ok\) await refreshHistory\(true\)/);
  assert.match(workspacePanel, /runHistoryMutation[\s\S]{0,140}"git_commit_action"/);
  assert.match(workspacePanel, /runHistoryMutation[\s\S]{0,140}"git_reset_to"/);
});

test("Git refreshes ignore stale requests and stale row closures", () => {
  assert.match(workspacePanel, /const sequence = \+\+gitRefreshSequence/);
  assert.match(workspacePanel, /sequence !== gitRefreshSequence/);
  assert.match(workspacePanel, /if \(opened && mode === "git" && !gitOperationRunning\)/);
  assert.match(workspacePanel, /main\.onclick = \(\) => void previewGitChange\(item\)/);
});

test("history pagination uses the last applied filter snapshot", () => {
  assert.match(workspacePanel, /let appliedHistoryFilters = historyFilterArgs/);
  assert.match(workspacePanel, /if \(reset\)\s*{\s*appliedHistoryFilters = historyFilterArgs/s);
  assert.match(workspacePanel, /\.\.\.appliedHistoryFilters/);
});

test("Git commands are non-interactive, bounded, and timed out", () => {
  assert.match(gitRunner, /GIT_TERMINAL_PROMPT/);
  assert.match(gitRunner, /GCM_INTERACTIVE/);
  assert.match(gitRunner, /try_wait\(\)/);
  assert.match(gitRunner, /child\.kill\(\)/);
  assert.match(gitRunner, /output exceeded/);
});
