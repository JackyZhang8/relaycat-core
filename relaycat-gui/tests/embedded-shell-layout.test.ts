import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const stylesSource = readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);
const mainSource = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");
const i18nSource = readFileSync(new URL("../src/i18n.ts", import.meta.url), "utf8");
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

test("files Git and Shell are separate right-panel entry modes", () => {
  assert.match(workspaceSource, /show\(mode: WorkspaceEntryMode\): void/);
  assert.match(workspaceSource, /panel\.classList\.toggle\("files-mode"/);
  assert.match(workspaceSource, /panel\.classList\.toggle\("git-mode"/);
  assert.match(workspaceSource, /panel\.classList\.toggle\("shell-mode"/);
  assert.match(workspaceSource, /stage\.classList\.toggle\("workspace-files-open"/);
  assert.match(workspaceSource, /stage\.classList\.toggle\(\s*"workspace-git-open"/);
  assert.match(workspaceSource, /stage\.classList\.toggle\("workspace-shell-open"/);
});

test("file Git and Shell panel state belongs to each main session", () => {
  assert.match(mainSource, /sidePanelMode:\s*"files" \| "git" \| "history" \| "shell" \| null/);
  assert.match(mainSource, /sidePanelMode:\s*null/);
  assert.match(mainSource, /applyTermSidePanel\(tab,\s*tab\?\.sidePanelMode \?\? null/);
  assert.match(mainSource, /toggleTermSidePanel\(tab\.sidePanelMode,\s*"files"\)/);
  assert.match(mainSource, /toggleTermSidePanel\(tab\.sidePanelMode,\s*"git"\)/);
  assert.match(mainSource, /toggleTermSidePanel\(tab\.sidePanelMode,\s*"shell"\)/);
  assert.match(mainSource, /embeddedShellPanel\?\.setVisible\(mode === "shell"\)/);
  assert.match(mainSource, /embeddedShellPanel\?\.activateForProject\(tab\.project\)/);
  assert.match(workspaceSource, /onModeChange:\s*\(mode: Exclude<WorkspaceEntryMode, null>\) => void/);
  assert.match(workspaceSource, /onModeChange\(mode\)/);
});

test("embedded Shell tabs live inside the unified workspace sidebar", () => {
  const workspaceAt = html.indexOf('id="workspace-panel"');
  const workspaceEnd = html.indexOf("</aside>", workspaceAt);
  const shellPanelAt = html.indexOf('id="embedded-shell-panel"');
  assert.ok(shellPanelAt > workspaceAt && shellPanelAt < workspaceEnd);
  assert.doesNotMatch(html, /id="embedded-shell-resizer"/);
  assert.match(html, /id="embedded-shell-tabs"/);
  assert.match(html, /id="embedded-shell-terminals"/);
  assert.doesNotMatch(html, /id="embedded-shell-close-all"/);
});

test("embedded Shell keeps Shell 1 shared and creates Shell 2 to 5 locally", () => {
  assert.match(panelSource, /const shells: EmbeddedShellTab\[\] = \[\]/);
  assert.match(panelSource, /session\.mode !== "relay"/);
  assert.match(panelSource, /kind:\s*"shared" \| "local"/);
  assert.match(panelSource, /ownerSessionId:\s*string/);
  assert.match(panelSource, /number:\s*number/);
  assert.match(panelSource, /const id = session\.id/);
  assert.match(panelSource, /nextLocalShellNumber\(session\.id, shells\)/);
  assert.match(panelSource, /workspace-local:\$\{session\.id\}/);
  assert.match(panelSource, /invoke<WorkspaceTerminalSnapshot>\("attach_workspace_terminal"/);
  assert.match(panelSource, /invoke<SessionInfo>\("create_session"/);
  assert.match(panelSource, /tool:\s*"shell"/);
  assert.match(panelSource, /relay:\s*null/);
  assert.doesNotMatch(panelSource, /pairingTabId|pairUrl|splitId/);
});

test("the per-session Shell limit uses the shared in-app dialog", () => {
  assert.match(panelSource, /notice:\s*\(message: string\) => Promise<void>/);
  assert.match(panelSource, /await options\.notice\(t\("embedded_shell_limit"\)\)/);
  assert.match(panelSource, /addBtn\.disabled = !activeOwnerSessionId;/);
  assert.doesNotMatch(panelSource, /term\.writeln\(`\\r\\n\\x1b\[33m\$\{t\("embedded_shell_limit"\)\}/);
  assert.match(mainSource, /hideCancel\?:\s*boolean/);
  assert.match(mainSource, /cancelBtn\.hidden = !!opts\.hideCancel/);
  assert.match(mainSource, /notice:\s*async \(message\) =>[\s\S]*?hideCancel:\s*true/);
});

test("the add button recreates a missing APP Shell before enforcing the limit", () => {
  assert.match(panelSource, /embeddedShellCreateKind\(session\.id, shells\)/);
  assert.match(panelSource, /case "shared":[\s\S]*?createForProject/);
  assert.match(panelSource, /case "local":[\s\S]*?createLocalForProject/);
  assert.match(panelSource, /case "limit":[\s\S]*?embedded_shell_limit/);
});

test("embedded Shells follow appearance settings and protect running processes on quit", () => {
  assert.match(panelSource, /activateForProject\(project: string \| null\): Promise<void>/);
  assert.match(panelSource, /setVisible\(visible: boolean\): void/);
  assert.match(panelSource, /runningCount\(\): number/);
  assert.match(panelSource, /updateAppearance\(theme: ITheme, fontSize: number\): void/);
  assert.match(panelSource, /closeForSession\(sessionId: string\): Promise<void>/);
  assert.match(mainSource, /embeddedShellPanel\?\.updateAppearance\(xtermTheme\(\), fontSize\(\)\)/);
  assert.match(mainSource, /embeddedShellPanel\?\.runningCount\(\)/);
  assert.match(mainSource, /await embeddedShellPanel\?\.closeForSession\(id\)/);
});

test("closing routes shared and local Shells to their own transports", () => {
  assert.match(panelSource, /await options\.confirm\(t\("embedded_shell_close_confirm"/);
  assert.match(panelSource, /if \(!confirmed\) return;\s*await closeShell\(id, true, true\)/);
  assert.match(panelSource, /terminate \? "close_workspace_terminal" : "detach_workspace_terminal"/);
  assert.match(panelSource, /invoke\("close_session", \{ id: shell\.id \}\)/);
  assert.match(panelSource, /invoke\("write_session", \{ id: shell\.id, data \}\)/);
  assert.match(panelSource, /invoke\("resize_session"/);
  assert.doesNotMatch(panelSource, /embedded_shell_close_all_confirm/);
  assert.match(mainSource, /confirm:\s*\(message\) =>/);
  assert.match(i18nSource, /embedded_shell_close_confirm:\s*"确定关闭共享终端“\{0\}”吗？其中正在运行的命令将被终止。"/);
  assert.match(i18nSource, /Close shared terminal "\{0\}"\? Any command running in it will be terminated\./);
});

test("APP closing the shared Shell removes its GUI tab and shows a notice", () => {
  assert.match(panelSource, /event\.payload\.kind === "exit"/);
  assert.match(panelSource, /finalizeShellRemoval\(shell\.id, true\)/);
  assert.match(panelSource, /options\.notice\(t\("embedded_shell_closed_in_app"\)\)/);
  assert.match(mainSource, /notice:\s*async \(message\) =>[\s\S]*?hideCancel:\s*true/);
  assert.match(i18nSource, /embedded_shell_closed_in_app:\s*"该共享终端已在 APP 端关闭"/);
  assert.match(i18nSource, /embedded_shell_closed_in_app:\s*"The shared terminal was closed in the APP\."/);
});

test("an APP-created shared Shell automatically adds the matching GUI tab", () => {
  assert.match(panelSource, /if \(!shell\) \{[\s\S]*?event\.payload\.kind !== "started"/);
  assert.match(panelSource, /const session = options\.currentSession\(\)/);
  assert.match(panelSource, /session\.id !== event\.payload\.id/);
  assert.match(panelSource, /void createForProject\(session\.project\)/);
  assert.doesNotMatch(panelSource, /event\.payload\.kind === "started"[\s\S]*?setVisible\(true\)/);
});

test("Shell groups remember ownership and serialize attach and close", () => {
  assert.match(panelSource, /state:\s*"launching" \| "running" \| "exited"/);
  assert.match(panelSource, /ready:\s*Promise<void>/);
  assert.match(panelSource, /closing:\s*boolean/);
  assert.match(panelSource, /await shell\.ready/);
  assert.match(panelSource, /activeOwnerSessionId/);
  assert.match(panelSource, /activeShellByOwner/);
  assert.match(panelSource, /shell\.ownerSessionId === activeOwnerSessionId/);
  assert.doesNotMatch(panelSource, /for \(const shell of \[\.\.\.shells\]\) removeShell/);
});

test("workspace terminal command failures are handled instead of becoming unhandled rejections", () => {
  assert.match(panelSource, /invoke\("resize_workspace_terminal"[\s\S]*?\.catch\(\(\) => \{\}\)/);
  assert.match(panelSource, /invoke\("write_workspace_terminal"[\s\S]*?\.catch\(\(\) => \{\}\)/);
  assert.match(panelSource, /invoke\(terminate \? "close_workspace_terminal" : "detach_workspace_terminal"[\s\S]*?\.catch\(\(\) => \{\}\)/);
  assert.match(panelSource, /listen<OutputEvent>\("session:\/\/output"/);
  assert.match(panelSource, /listen<StatusEvent>\("session:\/\/status"/);
});

test("workspace terminal attach orders replay before newer live output", () => {
  assert.match(panelSource, /last_output_seq:\s*number/);
  assert.match(panelSource, /output_seq\?:\s*number/);
  assert.match(panelSource, /pendingOutput/);
  assert.match(panelSource, /output_seq > snapshot\.last_output_seq/);
});

test("Term tool buttons stay bright without a right-side indicator", () => {
  const railRule = stylesSource.match(/\.term-toolrail\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const buttonRule = stylesSource.match(/\.term-toolbtn\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const hoverRule = stylesSource.match(
    /\.term-toolbtn:hover,[\s\S]*?\.term-toolbtn\.active\s*\{([\s\S]*?)\}/,
  )?.[1] ?? "";
  const activeRule = stylesSource.match(
    /\.stage\.workspace-files-open[\s\S]*?\.term-toolbtn\.shell\s*\{([\s\S]*?)\}/,
  )?.[1] ?? "";

  assert.match(railRule, /border:\s*0/);
  assert.match(railRule, /background:\s*transparent/);
  assert.match(railRule, /backdrop-filter:\s*none/);
  assert.match(buttonRule, /opacity:\s*0\.85/);
  assert.doesNotMatch(buttonRule, /border-right/);
  assert.doesNotMatch(hoverRule, /border-color/);
  assert.match(hoverRule, /box-shadow:\s*0 0 8px var\(--accent-ring\)/);
  assert.doesNotMatch(activeRule, /border-right-color/);
});

test("embedded Shell chrome follows the GUI theme while xterm stays dark", () => {
  const panelRule = stylesSource.match(/\.embedded-shell-panel\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const openPanelRule = stylesSource.match(
    /\.workspace-panel\.shell-mode \.embedded-shell-panel\s*\{([\s\S]*?)\}/,
  )?.[1] ?? "";
  const headerRule = stylesSource.match(/\.embedded-shell-header\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const terminalsRule = stylesSource.match(/\.embedded-shell-terminals\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const paneRule = stylesSource.match(/\.embedded-shell-pane\s*\{([\s\S]*?)\}/)?.[1] ?? "";

  assert.match(panelRule, /background:\s*var\(--panel\)/);
  assert.match(panelRule, /color:\s*var\(--text\)/);
  assert.match(openPanelRule, /display:\s*flex/);
  assert.match(openPanelRule, /flex-direction:\s*column/);
  assert.match(openPanelRule, /flex:\s*1/);
  assert.match(headerRule, /background:\s*var\(--panel\)/);
  assert.match(headerRule, /border-bottom:\s*1px solid var\(--border\)/);
  assert.match(terminalsRule, /background:\s*#0f0b09/);
  assert.match(paneRule, /background:\s*#0f0b09/);
  assert.doesNotMatch(stylesSource, /shell-dock-open|--embedded-shell-height/);
  assert.doesNotMatch(panelSource, /embeddedShellHeight|storedEmbeddedShellHeight/);
});

test("embedded Shell header omits the project path and keeps close buttons unobtrusive", () => {
  const closeRule = stylesSource.match(/\.embedded-shell-tab-close\s*\{([\s\S]*?)\}/)?.[1] ?? "";

  assert.doesNotMatch(html, /embedded-shell-heading|embedded-shell-path/);
  assert.doesNotMatch(panelSource, /pathEl/);
  assert.match(closeRule, /opacity:\s*0/);
  assert.match(
    stylesSource,
    /\.embedded-shell-tab:hover \.embedded-shell-tab-close,[\s\S]*?\.embedded-shell-tab\.active \.embedded-shell-tab-close\s*\{[\s\S]*?opacity:\s*1/,
  );
});

test("embedded Shell tabs have left and right overflow controls", () => {
  assert.match(html, /id="embedded-shell-scroll-left"/);
  assert.match(html, /id="embedded-shell-scroll-right"/);
  assert.match(panelSource, /import \{ tabScrollState \} from "\.\/tab-scroll"/);
  assert.match(panelSource, /tabScrollState\(tabsEl\)/);
  assert.match(panelSource, /tabsEl\.scrollBy\(/);
  assert.match(panelSource, /tabsEl\.addEventListener\("scroll"/);
  assert.match(panelSource, /new ResizeObserver\([^)]*updateShellTabScrollControls/s);
  assert.match(stylesSource, /\.embedded-shell-scroll\s*\{/);
});

test("the active embedded Shell tab uses a top orange indicator", () => {
  const tabRule = stylesSource.match(/\.embedded-shell-tab\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const activeRule = stylesSource.match(/\.embedded-shell-tab\.active\s*\{([\s\S]*?)\}/)?.[1] ?? "";

  assert.match(tabRule, /border-top:\s*2px solid transparent/);
  assert.doesNotMatch(tabRule, /border-bottom:\s*2px/);
  assert.match(activeRule, /border-top-color:\s*var\(--accent\)/);
  assert.doesNotMatch(activeRule, /border-bottom-color/);
});
