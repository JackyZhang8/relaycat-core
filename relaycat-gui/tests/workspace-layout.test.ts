import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { workspacePreviewStartsCollapsed } from "../src/workspace-model.ts";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const workspacePanel = readFileSync(
  new URL("../src/workspace-panel.ts", import.meta.url),
  "utf8",
);

test("git changes starts with the preview collapsed", () => {
  assert.equal(workspacePreviewStartsCollapsed("git"), true);
  assert.equal(workspacePreviewStartsCollapsed("files"), false);
  assert.equal(workspacePreviewStartsCollapsed("history"), false);
});

test("git history scrolls long rows without stretching its filters", () => {
  assert.match(
    css,
    /#workspace-history-view:not\(\[hidden\]\)\s*{[^}]*overflow:\s*hidden;/s,
  );
  assert.match(css, /\.ws-history-list\s*{[^}]*overflow:\s*auto;/s);
  assert.match(css, /\.ws-history-filters\s*{[^}]*width:\s*100%;[^}]*min-width:\s*0;/s);
});

test("git history flex layout applies only while the history tab is visible", () => {
  assert.match(css, /#workspace-history-view:not\(\[hidden\]\)\s*{[^}]*display:\s*flex;/s);
  assert.doesNotMatch(css, /#workspace-history-view\s*{[^}]*display:\s*flex;/s);
});

test("commit details scroll in both directions without widening the history layout", () => {
  assert.match(css, /\.ws-main\s*{[^}]*min-width:\s*0;/s);
  assert.match(
    css,
    /\.ws-preview\s*{[^}]*min-width:\s*0;[^}]*overflow:\s*hidden;/s,
  );
  assert.match(
    css,
    /\.ws-preview-code\s*{[^}]*width:\s*100%;[^}]*max-width:\s*100%;[^}]*overflow-x:\s*auto;[^}]*overflow-y:\s*auto;/s,
  );
});

test("the Git changes toolbar omits low-frequency branch management buttons", () => {
  assert.doesNotMatch(html, /id="workspace-new-branch"/);
  assert.doesNotMatch(html, /id="workspace-rename-branch"/);
  assert.doesNotMatch(html, /id="workspace-delete-branch"/);
});

test("file previews own their horizontal and vertical scrolling", () => {
  assert.match(
    css,
    /\.ws-preview-code\s*{[^}]*overflow-x:\s*auto;[^}]*overflow-y:\s*auto;[^}]*overscroll-behavior:\s*contain;/s,
  );
  assert.match(css, /\.ws-code-line\s*{[^}]*width:\s*max-content;[^}]*min-width:\s*100%;/s);
});

test("remote Git output stays inside a scrollable operation log", () => {
  assert.match(
    css,
    /\.ws-operation-log\s*{[^}]*max-height:[^;]+;[^}]*overflow:\s*auto;/s,
  );
  assert.match(css, /\.ws-operation-progress\s*{[^}]*overflow:\s*hidden;/s);
  assert.match(css, /\.ws-operation\.success\s+\.ws-operation-progress-bar/s);
});

test("manual workspace refresh shows loading progress and completion feedback", () => {
  assert.match(html, /id="workspace-refresh-progress"[^>]*aria-live="polite"/);
  assert.match(css, /\.workspace-panel\.refreshing\s+\.ws-refresh-progress/s);
  assert.match(css, /\.workspace-panel\.refreshed\s+\.ws-refresh-progress-bar/s);
  assert.match(
    css,
    /\.ws-refresh-progress-bar\s*{[^}]*background:\s*linear-gradient\([^;]+#ff3b30[^;]+#ffcc00[^;]+#00e676[^;]+#00c7ff[^;]+#a855f7[^;]+#ff2d92[^;]+\);/s,
  );
  assert.match(workspacePanel, /setRefreshVisual\("running"\)/);
  assert.match(workspacePanel, /setRefreshVisual\("complete"\)/);
  assert.match(workspacePanel, /setRefreshVisual\("error"\)/);
  assert.match(css, /\.workspace-panel\.refresh-error\s+\.ws-refresh-progress-bar/s);
});

test("preview height is resizable and survives collapse and expand", () => {
  assert.match(html, /id="workspace-preview-resizer"/);
  assert.match(css, /\.ws-preview-resizer\s*{[^}]*cursor:\s*row-resize;/s);
  assert.match(
    workspacePanel,
    /workspaceMain\.style\.setProperty\("--workspace-preview-height"/,
  );
  assert.match(workspacePanel, /previewResizer\.addEventListener\("pointermove"/);
  assert.doesNotMatch(
    workspacePanel,
    /setPreviewCollapsed[\s\S]{0,400}removeProperty\("--workspace-preview-height"/,
  );
});
