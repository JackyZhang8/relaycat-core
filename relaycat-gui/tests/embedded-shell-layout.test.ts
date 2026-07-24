import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const mainSource = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");
const workspaceSource = readFileSync(
  new URL("../src/workspace-panel.ts", import.meta.url),
  "utf8",
);
const panelSource = readFileSync(
  new URL("../src/embedded-shell-panel.ts", import.meta.url),
  "utf8",
);

test("file Git and terminal tools belong to the focused Term", () => {
  assert.doesNotMatch(html, /id="btn-workspace"/);
  assert.match(mainSource, /className = "term-toolrail"/);
  assert.match(mainSource, /term_tool_files/);
  assert.match(mainSource, /term_tool_git/);
  assert.match(mainSource, /term_tool_shell/);
});

test("files and Git are separate right-panel entry modes", () => {
  assert.match(workspaceSource, /show\(mode: WorkspaceEntryMode\): void/);
  assert.match(workspaceSource, /panel\.classList\.toggle\("files-mode"/);
  assert.match(workspaceSource, /panel\.classList\.toggle\("git-mode"/);
  assert.match(workspaceSource, /stage\.classList\.toggle\("workspace-files-open"/);
  assert.match(workspaceSource, /stage\.classList\.toggle\("workspace-git-open"/);
});

test("file and Git panel state belongs to each main session", () => {
  assert.match(mainSource, /sidePanelMode:\s*"files" \| "git" \| "history" \| null/);
  assert.match(mainSource, /sidePanelMode:\s*null/);
  assert.match(mainSource, /workspacePanel\?\.show\(tab\?\.sidePanelMode \?\? null\)/);
  assert.match(mainSource, /tab\.sidePanelMode = toggleTermSidePanel/);
  assert.match(workspaceSource, /onModeChange:\s*\(mode: Exclude<WorkspaceEntryMode, null>\) => void/);
  assert.match(workspaceSource, /onModeChange\(mode\)/);
});

test("embedded Shell tabs live outside the pairing tab strip", () => {
  const mainTabsAt = html.indexOf('id="tabbar"');
  const shellPanelAt = html.indexOf('id="embedded-shell-panel"');
  const terminalStageAt = html.indexOf('id="terminal-stage"');
  assert.ok(shellPanelAt > mainTabsAt);
  assert.ok(shellPanelAt > terminalStageAt);
  assert.match(html, /id="embedded-shell-tabs"/);
  assert.match(html, /id="embedded-shell-terminals"/);
});

test("embedded Shells use their own local PTY collection", () => {
  assert.match(panelSource, /const shells: EmbeddedShellTab\[\] = \[\]/);
  assert.match(panelSource, /embedded-shell-/);
  assert.match(panelSource, /tool:\s*"shell"/);
  assert.match(panelSource, /relay:\s*null/);
  assert.match(panelSource, /MAX_EMBEDDED_SHELLS = 8/);
  assert.doesNotMatch(panelSource, /pairingTabId|pairUrl|splitId/);
});

test("embedded Shells follow appearance settings and protect running processes on quit", () => {
  assert.match(panelSource, /runningCount\(\): number/);
  assert.match(panelSource, /updateAppearance\(theme: ITheme, fontSize: number\): void/);
  assert.match(mainSource, /embeddedShellPanel\?\.updateAppearance\(xtermTheme\(\), fontSize\(\)\)/);
  assert.match(mainSource, /embeddedShellPanel\?\.runningCount\(\)/);
});

test("closing one or all embedded Shells requires confirmation", () => {
  assert.match(panelSource, /await options\.confirm\(t\("embedded_shell_close_confirm"/);
  assert.match(panelSource, /await options\.confirm\(t\("embedded_shell_close_all_confirm"/);
  assert.match(mainSource, /confirm:\s*\(message\) =>/);
});

test("Shell launch and close are serialized without bulk-close focus races", () => {
  assert.match(panelSource, /state:\s*"launching" \| "running" \| "exited"/);
  assert.match(panelSource, /ready:\s*Promise<void>/);
  assert.match(panelSource, /closing:\s*boolean/);
  assert.match(panelSource, /await shell\.ready/);
  assert.match(panelSource, /closeShell\(id, false\)/);
  assert.doesNotMatch(panelSource, /for \(const shell of \[\.\.\.shells\]\) removeShell/);
});

test("PTY command failures are handled instead of becoming unhandled rejections", () => {
  assert.match(panelSource, /invoke\("resize_session"[\s\S]*?\.catch\(\(\) => \{\}\)/);
  assert.match(panelSource, /invoke\("write_session"[\s\S]*?\.catch\(\(\) => \{\}\)/);
  assert.match(panelSource, /invoke\("close_session"[\s\S]*?\.catch\(\(\) => \{\}\)/);
});
