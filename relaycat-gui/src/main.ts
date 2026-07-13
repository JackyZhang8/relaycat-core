import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { check, type Update, type DownloadEvent } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  enable as enableAutostart,
  disable as disableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { I18N, type Lang } from "./i18n";
import { MAX_TAB_COUNT, canCreateTab } from "./tab-limit";
import { tabScrollState } from "./tab-scroll";
import thirdPartyLicenses from "./third-party-licenses.txt?raw";

type Tool = { name: string; label: string; kind: string };
type ConfigDto = {
  default_relay?: string | null;
  default_tool?: string | null;
  favorites: string[];
  tools: { name: string; label?: string | null; cmd: string; args: string[] }[];
  return_to_launcher: boolean;
};
type Recent = { id: string; kind: string; project: string; relay: string };
type SessionInfo = { id: string; title: string; mode: string; log_path?: string | null };
type OutputEvent = { id: string; data: number[] };
type StatusEvent = { id: string; state: string; code?: number };
type PairingEvent = { id: string; url: string };
type RelayEvent = {
  id: string;
  state: string;
  code?: string;
  retryable?: boolean;
  message?: string;
};
type Diag = {
  relay_engine: string;
  gui_exe: string;
  cli_version: string;
  config_path: string;
  recent_path: string;
};

type TabState = "local" | "wait" | "syncing" | "paired" | "exited";

interface Tab {
  id: string;
  tool: string;
  title: string;
  mode: string;
  state: TabState;
  project: string;
  relay: string;
  logPath?: string;
  peers: number;
  term: Terminal;
  fit: FitAddon;
  webgl?: WebglAddon;
  pane: HTMLDivElement;
  tabEl: HTMLDivElement;
  pairUrl?: string;
  // Negotiated child PTY grid pushed by the CLI over the Windows pipe bridge
  // (private OSC 9780). When set, the terminal is pinned to this size and the
  // extra desktop-window space is letterboxed, so the desktop renders the same
  // layout as the phone (which is what a full-screen TUI drew for). Unset on
  // macOS/Linux and for local sessions, where the terminal just fits the window.
  remoteGrid?: { cols: number; rows: number };
}

/* ------------------------------- theming --------------------------------- */

type Theme = "dark" | "light";

const THEMES: Record<Theme, { vars: Record<string, string>; xterm: ITheme }> = {
  dark: {
    // Deep coffee surfaces matching the relaycat.cn site (warm near-black).
    vars: {
      "--bg": "#0f0b09",
      "--panel": "#1b130e",
      "--panel2": "#251a13",
      "--border": "#3a2b20",
      "--text": "#fbf4ec",
      "--dim": "#b9a99a",
      "--topbar": "#17110d",
    },
    xterm: {
      background: "#0f0b09",
      foreground: "#fbf4ec",
      cursor: "#f2783f",
      selectionBackground: "#3a2b20",
      black: "#0f0b09",
      red: "#f94d3a",
      green: "#4ade80",
      yellow: "#fbbf24",
      blue: "#38bdf8",
      magenta: "#a78bfa",
      cyan: "#22d3ee",
      white: "#fbf4ec",
    },
  },
  light: {
    // Light coffee/cream surfaces matching the relaycat.cn site.
    vars: {
      "--bg": "#f6f1e8",
      "--panel": "#fffdf8",
      "--panel2": "#efe6d7",
      "--border": "#e2d7c4",
      "--text": "#17120f",
      "--dim": "#6f6358",
      "--topbar": "#f0e8da",
    },
    xterm: {
      background: "#fffdf8",
      foreground: "#17120f",
      cursor: "#df601d",
      selectionBackground: "#f0dcc4",
      black: "#17120f",
      red: "#e63024",
      green: "#1a7f37",
      yellow: "#9a6700",
      blue: "#0969da",
      magenta: "#8250df",
      cyan: "#1b7c83",
      white: "#6f6358",
    },
  },
};

let theme: Theme = (localStorage.getItem("relaycat.theme") as Theme) || "dark";

// Mainstream sun / moon glyphs (Lucide-style line icons) drawn with currentColor
// so they pick up the button's text color in either theme.
const ICON_SUN =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>';
const ICON_MOON =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>';

function applyTheme() {
  const def = THEMES[theme] ? theme : "dark";
  const root = document.documentElement;
  for (const [k, v] of Object.entries(THEMES[def].vars)) root.style.setProperty(k, v);
  document.body.dataset.theme = def;
  // The terminal region always uses the dark palette regardless of the app's
  // light/dark chrome: a light xterm background shows through the cells a TUI
  // (e.g. opencode) doesn't paint, leaving a two-tone light/dark background.
  // Keeping the terminal dark matches what the tools expect and the phone app.
  for (const tab of tabs) tab.term.options.theme = xtermTheme();
  const tbtn = document.getElementById("btn-theme");
  // Show the icon of the mode you'd switch *to*: a sun while in dark, a moon
  // while in light.
  if (tbtn) tbtn.innerHTML = def === "dark" ? ICON_SUN : ICON_MOON;
  const sel = document.getElementById("set-theme") as HTMLSelectElement | null;
  if (sel) sel.value = def;
}

function setTheme(next: Theme) {
  theme = THEMES[next] ? next : "dark";
  localStorage.setItem("relaycat.theme", theme);
  applyTheme();
}

function toggleTheme() {
  setTheme(theme === "dark" ? "light" : "dark");
}

// The terminal is always rendered with the dark palette (see `applyTheme`),
// independent of the app's selected light/dark chrome.
function xtermTheme(): ITheme {
  return THEMES.dark.xterm;
}

/* --------------------------------- i18n ---------------------------------- */

let lang: Lang = (localStorage.getItem("relaycat.lang") as Lang) || "zh";

function t(key: string, ...args: (string | number)[]): string {
  const table = I18N[I18N[lang] ? lang : "zh"];
  let s = table[key] ?? I18N.zh[key] ?? key;
  args.forEach((a, i) => {
    s = s.replace(new RegExp(`\\{${i}\\}`, "g"), String(a));
  });
  return s;
}

function applyI18n() {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((node) => {
    const key = node.dataset.i18n!;
    node.textContent = t(key);
  });
  document.querySelectorAll<HTMLElement>("[data-i18n-ph]").forEach((node) => {
    (node as HTMLInputElement).placeholder = t(node.dataset.i18nPh!);
  });
  document.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((node) => {
    node.title = t(node.dataset.i18nTitle!);
  });
}

function setLanguage(next: Lang) {
  lang = next;
  localStorage.setItem("relaycat.lang", next);
  applyI18n();
  for (const tab of tabs) refreshTabEl(tab);
  renderStatusbar();
}

/* -------------------------------- state ---------------------------------- */

const tabs: Tab[] = [];
let activeId: string | null = null;
let splitId: string | null = null;
let splitOn = false;
let tools: Tool[] = [];
// Per-tool install status (name -> installed), read from the persisted
// detection cache (`cached_tool_status`). A missing entry is treated as
// installed so tools stay selectable until a detection sweep has run.
let toolInstalled: Record<string, boolean> = {};
let config: ConfigDto = {
  favorites: [],
  tools: [],
  return_to_launcher: true,
};
let selectedTool = "shell";
let selectedProject = "";
let pairingTabId: string | null = null;
let diagTimer: number | null = null;
let pairCountdownTimer: number | null = null;
let tabMenuEl: HTMLDivElement | null = null;

function clearPairCountdown() {
  if (pairCountdownTimer !== null) {
    clearInterval(pairCountdownTimer);
    pairCountdownTimer = null;
  }
}

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;
const el = (tag: string, cls?: string) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  return node;
};

function fontSize(): number {
  return Number(localStorage.getItem("relaycat.fontSize") || "14");
}

/* ------------------------------- overlays -------------------------------- */

function closeOverlays() {
  document.querySelectorAll(".overlay").forEach((o) => o.classList.remove("show"));
  if (diagTimer !== null) {
    clearInterval(diagTimer);
    diagTimer = null;
  }
  clearPairCountdown();
}

function openOverlay(id: string) {
  closeOverlays();
  $(`#${id}`).classList.add("show");
}

// Dismiss the topmost overlay. Overlays tagged with data-parent (e.g. the
// devices / clear-data / diagnostics dialogs opened from Settings) return to
// their parent instead of closing everything outright.
function dismissOverlay(ov: HTMLElement | null) {
  const parent = ov?.dataset.parent;
  if (parent) {
    openOverlay(parent);
  } else {
    closeOverlays();
    // Back on the bare landing: a good moment to show the one-time coach marks
    // (e.g. right after the first-run wizard or new-session dialog is closed).
    maybeShowCoachMarks();
  }
}

// Close a dialog from its "×" button, mapping to the dialog's own dismissal
// semantics: the confirm dialog resolves as a cancel, onboarding is marked seen
// (like "skip"), everything else uses the shared dismiss logic.
function closeDialog(ov: HTMLElement) {
  if (ov.id === "ov-confirm") {
    ($("#confirm-cancel") as HTMLButtonElement).click();
  } else if (ov.id === "ov-onboard") {
    skipOnboarding();
  } else {
    dismissOverlay(ov);
  }
}

// Styled in-app confirmation dialog. Returns a promise that resolves to true
// when the user confirms and false on cancel / backdrop click / Escape.
function confirmDialog(opts: {
  title: string;
  message: string;
  okLabel?: string;
  danger?: boolean;
}): Promise<boolean> {
  return new Promise((resolve) => {
    const ov = $("#ov-confirm");
    $("#confirm-title").textContent = opts.title;
    $("#confirm-message").textContent = opts.message;
    const okBtn = $("#confirm-ok") as HTMLButtonElement;
    const cancelBtn = $("#confirm-cancel") as HTMLButtonElement;
    okBtn.textContent = opts.okLabel ?? t("confirm_ok");
    cancelBtn.textContent = t("cancel");
    okBtn.className = `btn ${opts.danger ? "danger" : "primary"}`;

    const onBackdrop = (ev: MouseEvent) => {
      if (ev.target === ov) cleanup(false);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        cleanup(false);
      } else if (ev.key === "Enter") {
        ev.preventDefault();
        cleanup(true);
      }
    };
    function cleanup(val: boolean) {
      ov.classList.remove("show");
      okBtn.onclick = null;
      cancelBtn.onclick = null;
      ov.removeEventListener("click", onBackdrop);
      window.removeEventListener("keydown", onKey, true);
      resolve(val);
    }

    okBtn.onclick = () => cleanup(true);
    cancelBtn.onclick = () => cleanup(false);
    ov.addEventListener("click", onBackdrop);
    window.addEventListener("keydown", onKey, true);
    ov.classList.add("show");
    okBtn.focus();
  });
}

/* ----------------------------- new session ------------------------------- */

let projectRows: { path: string; favorite: boolean; note?: string }[] = [];

function ensureTabCapacity(): boolean {
  if (canCreateTab(tabs.length)) return true;
  alert(t("tab_limit_reached", MAX_TAB_COUNT));
  return false;
}

async function openNewSession() {
  if (!ensureTabCapacity()) return;
  config = await invoke<ConfigDto>("get_config");
  tools = await invoke<Tool[]>("list_tools");
  selectedTool = config.default_tool || tools[0]?.name || "shell";

  // Render chips immediately so the dialog opens without waiting, then disable
  // any tools the persisted detection cache marks as not installed.
  renderToolChips();
  void refreshNewSessionToolStatus();

  // Unlike the CLI, the GUI is an installed app whose working directory (e.g.
  // the install location) is meaningless as a project, so the list is built
  // only from favorites and recent sessions — empty until the user adds one.
  const recents = await invoke<Recent[]>("list_recents");

  projectRows = [];
  for (const fav of config.favorites) projectRows.push({ path: fav, favorite: true });
  for (const r of recents) {
    if (!projectRows.some((p) => p.path === r.project))
      projectRows.push({ path: r.project, favorite: false });
  }
  selectedProject = projectRows[0]?.path || "";

  renderProjectList();
  ($("#proj-input") as HTMLInputElement).value = "";
  // Pre-fill the relay: explicit default first, otherwise the most recently
  // used relay so re-launching a session doesn't silently drop into a local
  // shell when the user forgets to re-type the relay.
  const lastRelay = recents.find((r) => r.relay)?.relay;
  ($("#relay-input") as HTMLInputElement).value =
    config.default_relay || lastRelay || "";
  syncLaunchValidity();

  openOverlay("ov-new");
}

// Build the tool chips for the new-session dialog. Tools detected as not
// installed are shown disabled (can't be selected as the session tool) so a
// session can't be launched against a missing program.
function renderToolChips() {
  const chips = $("#tool-chips");
  chips.innerHTML = "";
  for (const tool of tools) {
    const installed = toolInstalled[tool.name] !== false;
    const chip = el("div", "chip") as HTMLDivElement;
    chip.textContent = tool.label;
    if (tool.name === selectedTool) chip.classList.add("sel");
    if (installed) {
      chip.onclick = () => {
        selectedTool = tool.name;
        chips.querySelectorAll(".chip").forEach((c) => c.classList.remove("sel"));
        chip.classList.add("sel");
      };
    } else {
      chip.classList.add("disabled");
      chip.title = t("ns_tool_not_installed");
    }
    chips.appendChild(chip);
  }
}

// Load tool availability from the persisted detection cache (no live probing).
// Live detection only runs from the onboarding/settings detection page; it
// updates the cache, which this reads.
async function refreshToolStatus() {
  try {
    const statuses = await invoke<ToolStatus[]>("cached_tool_status");
    toolInstalled = statusMap(statuses);
  } catch (e) {
    console.error(e);
  }
}

function statusMap(statuses: ToolStatus[]): Record<string, boolean> {
  const map: Record<string, boolean> = {};
  for (const s of statuses) map[s.name] = s.installed;
  return map;
}

// Refresh install status for the new-session dialog and re-render the chips.
// If the pre-selected tool turns out to be missing, fall back to the first
// installed one so the launch button never targets a missing program.
async function refreshNewSessionToolStatus() {
  await refreshToolStatus();
  if (toolInstalled[selectedTool] === false) {
    const firstInstalled = tools.find((tl) => toolInstalled[tl.name] !== false);
    if (firstInstalled) selectedTool = firstInstalled.name;
  }
  renderToolChips();
}

// Disable the not-installed entries in the settings "default tool" dropdown so
// a missing tool can't be chosen as the default. Uses the current
// `toolInstalled` map (already loaded by the caller).
function applyToolStatusToSettings() {
  const toolSel = $("#set-default-tool") as HTMLSelectElement | null;
  if (!toolSel) return;
  for (const opt of Array.from(toolSel.options)) {
    const missing = toolInstalled[opt.value] === false;
    opt.disabled = missing;
    const label = tools.find((tl) => tl.name === opt.value)?.label ?? opt.value;
    opt.textContent = missing ? `${label} · ${t("ob_tool_missing")}` : label;
  }
}

async function refreshSettingsToolOptions() {
  await refreshToolStatus();
  applyToolStatusToSettings();
}

function renderProjectList() {
  const projList = $("#proj-list");
  projList.innerHTML = "";
  if (projectRows.length === 0) {
    const note = el("div", "empty-note");
    note.textContent = t("proj_list_empty");
    projList.appendChild(note);
    return;
  }
  for (const p of projectRows) {
    const row = el("div", "row") as HTMLDivElement;
    if (p.path === selectedProject) row.classList.add("sel");

    const star = el("span", p.favorite ? "star on" : "star") as HTMLSpanElement;
    star.textContent = p.favorite ? "★" : "☆";
    star.title = p.favorite ? t("unfavorite") : t("favorite");
    star.onclick = (ev) => {
      ev.stopPropagation();
      void toggleFavorite(p.path);
    };
    row.appendChild(star);

    const label = el("span", p.note ? "muted" : "");
    label.textContent = p.note ? `${p.path}  (${p.note})` : p.path;
    row.appendChild(label);

    const rm = el("span", "rm") as HTMLSpanElement;
    rm.textContent = "✕";
    rm.title = t("delete");
    rm.onclick = (ev) => {
      ev.stopPropagation();
      void removeProjectRow(p.path);
    };
    row.appendChild(rm);

    row.onclick = () => {
      selectedProject = p.path;
      ($("#proj-input") as HTMLInputElement).value = "";
      projList.querySelectorAll(".row").forEach((r) => r.classList.remove("sel"));
      row.classList.add("sel");
      syncLaunchValidity();
    };
    projList.appendChild(row);
  }
}

// Remove a project from the picker list after confirmation: unstar it (if
// favorited) and forget the recent sessions it came from, so it doesn't
// reappear the next time the dialog opens.
async function removeProjectRow(path: string) {
  const ok = await confirmDialog({
    title: t("proj_remove_title"),
    message: t("proj_remove_confirm", path),
    okLabel: t("delete"),
    danger: true,
  });
  if (!ok) return;
  config = await invoke<ConfigDto>("get_config");
  if (config.favorites.includes(path)) {
    config.favorites = config.favorites.filter((f) => f !== path);
    await invoke("save_config", { config }).catch((e) => console.error(e));
  }
  const recents = await invoke<Recent[]>("list_recents").catch(() => [] as Recent[]);
  for (const r of recents.filter((r) => r.project === path)) {
    await invoke("forget_recent", { id: r.id }).catch(() => {});
  }
  projectRows = projectRows.filter((p) => p.path !== path);
  if (selectedProject === path) selectedProject = projectRows[0]?.path || "";
  renderProjectList();
  syncLaunchValidity();
}

async function toggleFavorite(path: string) {
  config = await invoke<ConfigDto>("get_config");
  const has = config.favorites.includes(path);
  config.favorites = has
    ? config.favorites.filter((f) => f !== path)
    : [...config.favorites, path];
  await invoke("save_config", { config }).catch((e) => console.error(e));
  const existing = projectRows.find((p) => p.path === path);
  if (existing) existing.favorite = !has;
  else if (!has) projectRows.unshift({ path, favorite: true });
  renderProjectList();
}

async function browseProject() {
  const picked = await openDialog({ directory: true, multiple: false }).catch(() => null);
  if (typeof picked === "string") {
    ($("#proj-input") as HTMLInputElement).value = picked;
    selectedProject = picked;
    $("#proj-list")
      .querySelectorAll(".row")
      .forEach((r) => r.classList.remove("sel"));
    syncLaunchValidity();
  }
}

// Relay and project are required: keep the launch button disabled and flag
// the empty field so a session can never silently fall back to a local shell
// or to the GUI process's working directory (the install location).
function syncLaunchValidity() {
  const relayInput = $("#relay-input") as HTMLInputElement;
  const projInput = $("#proj-input") as HTMLInputElement;
  const btn = $("#btn-launch") as HTMLButtonElement;
  const relayEmpty = relayInput.value.trim().length === 0;
  const projEmpty = projInput.value.trim().length === 0 && !selectedProject;
  btn.disabled = relayEmpty || projEmpty;
  relayInput.classList.toggle("invalid", relayEmpty);
  relayInput.title = relayEmpty ? t("ns_relay_required") : "";
  projInput.classList.toggle("invalid", projEmpty);
  projInput.title = projEmpty ? t("ns_project_required") : "";
}

async function launchSession() {
  const relay = ($("#relay-input") as HTMLInputElement).value.trim();
  const projInput = ($("#proj-input") as HTMLInputElement).value.trim();
  const project = projInput || selectedProject;
  if (!relay || !project) {
    syncLaunchValidity();
    ($(!relay ? "#relay-input" : "#proj-input") as HTMLInputElement).focus();
    return;
  }
  if (!ensureTabCapacity()) return;
  closeOverlays();
  await createTab(selectedTool, project, relay);
}

/* -------------------------------- tabs ----------------------------------- */

async function createTab(tool: string, project: string, relay: string) {
  if (!canCreateTab(tabs.length)) return;
  const term = new Terminal({
    fontFamily: '"JetBrains Mono", Menlo, Consolas, monospace',
    fontSize: fontSize(),
    cursorBlink: true,
    theme: xtermTheme(),
    // Keep tool-drawn text legible even when a tool paints a light input box
    // (e.g. Codex's composer) that would otherwise blend into the default
    // foreground under the dark theme. Matches VS Code's terminal default.
    minimumContrastRatio: 4.5,
    allowProposedApi: true,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon());

  const pane = el("div", "term-pane") as HTMLDivElement;
  $("#terminals").appendChild(pane);
  term.open(pane);

  let webgl: WebglAddon | undefined;
  try {
    webgl = new WebglAddon();
    webgl.onContextLoss(() => webgl?.dispose());
    term.loadAddon(webgl);
  } catch {
    webgl = undefined; // fall back to the DOM renderer
  }
  fit.fit();

  const id = `s-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
  const tab: Tab = {
    id,
    tool,
    title: tool,
    mode: relay ? "relay" : "local",
    state: relay ? "wait" : "local",
    project,
    relay,
    peers: 0,
    term,
    fit,
    webgl,
    pane,
    tabEl: el("div", "tab") as HTMLDivElement,
  };
  tabs.push(tab);
  buildTabEl(tab);
  selectTab(tab.id);
  renderEmpty();

  pane.addEventListener("contextmenu", (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    if (splitOn && tab.id === splitId) selectTab(tab.id);
    openTermMenu(tab, ev.clientX, ev.clientY);
  });
  // In split view, mousedown on the non-focused pane moves focus to it.
  pane.addEventListener("mousedown", () => {
    if (splitOn && tab.id === splitId) selectTab(tab.id);
  });

  // xterm 6.0.0 still defers IME punctuation reported as keydown(229) to a
  // zero-delay textarea read. Some desktop WebViews commit the punctuation
  // after that timer, so the first key is held until the next keydown. Keep a
  // baseline and flush the not-yet-sent textarea delta on keyup. This mirrors
  // the upstream xterm fix for Chinese IME punctuation (not yet released).
  let pending229Baseline: string | undefined;
  let imeComposing = false;
  let pending229FlushTimer: ReturnType<typeof setTimeout> | undefined;
  let suppressNext229AfterComposition = false;
  const modifierCodes = new Set([
    "ShiftLeft",
    "ShiftRight",
    "ControlLeft",
    "ControlRight",
    "AltLeft",
    "AltRight",
    "MetaLeft",
    "MetaRight",
  ]);
  const modifierKeysDown = new Set<string>();
  type EarlyImeInput = {
    data: string;
    consumed: boolean;
    awaitingKeydown: boolean;
  };
  const earlyImeInputs: EarlyImeInput[] = [];
  let lastOnData:
    | { data: string; textareaLength: number; at: number }
    | undefined;
  const trackModifier = (ev: KeyboardEvent, pressed: boolean) => {
    const keyId = ev.code || ev.key;
    if (!modifierCodes.has(keyId)) return;
    if (pressed) modifierKeysDown.add(keyId);
    else modifierKeysDown.delete(keyId);
  };
  term.textarea?.addEventListener("keydown", (ev) => trackModifier(ev, true), true);
  term.textarea?.addEventListener("keyup", (ev) => trackModifier(ev, false), true);
  const textareaDelta = (oldValue: string, newValue: string): string => {
    let prefix = 0;
    while (
      prefix < oldValue.length &&
      prefix < newValue.length &&
      oldValue.charCodeAt(prefix) === newValue.charCodeAt(prefix)
    ) {
      prefix++;
    }
    const removed = oldValue.length - prefix;
    return `${"\x7f".repeat(removed)}${newValue.substring(prefix)}`;
  };
  const flushPending229 = () => {
    if (pending229Baseline === undefined || imeComposing) return;
    const value = term.textarea?.value ?? "";
    const data = textareaDelta(pending229Baseline, value);
    if (!data) return;
    pending229Baseline = undefined;
    term.input(data, true);
  };
  const schedulePending229Flush = () => {
    if (pending229FlushTimer !== undefined) return;
    // Run after xterm's own zero-delay check so an xterm send can clear the
    // baseline before this fallback runs.
    pending229FlushTimer = setTimeout(() => {
      pending229FlushTimer = undefined;
      flushPending229();
    }, 0);
  };
  const queueEarlyImeInput = (data: string) => {
    const textareaLength = term.textarea?.value.length ?? -1;
    const now = performance.now();
    // Depending on WebKit's listener ordering, xterm may fire onData during
    // the input event before this listener runs. Match both the character and
    // the committed textarea length so a prior keypress cannot consume the
    // current candidate, even when the same character is typed repeatedly.
    const alreadySent =
      lastOnData !== undefined &&
      lastOnData.data === data &&
      lastOnData.textareaLength === textareaLength &&
      now - lastOnData.at <= 50;
    const candidate: EarlyImeInput = {
      data,
      consumed: alreadySent,
      awaitingKeydown: true,
    };
    earlyImeInputs.push(candidate);
    window.setTimeout(() => {
      if (!candidate.consumed) {
        candidate.consumed = true;
        term.input(data, true);
      }
    }, 16);
    window.setTimeout(() => {
      const index = earlyImeInputs.indexOf(candidate);
      if (index >= 0) earlyImeInputs.splice(index, 1);
    }, 500);
  };
  term.attachCustomKeyEventHandler((ev) => {
    if (ev.type === "keydown" && ev.keyCode === 229) {
      if (imeComposing || ev.isComposing) {
        // Real IME composition is fully owned by xterm's CompositionHelper.
        // A 229 baseline here would later replay part of the committed text.
        pending229Baseline = undefined;
      } else if (suppressNext229AfterComposition) {
        // WebKit emits the Enter/Space confirmation key after compositionend.
        // Its text has already been (or is about to be) emitted by xterm.
        suppressNext229AfterComposition = false;
        pending229Baseline = undefined;
      } else {
        const earlyInput = earlyImeInputs.find((candidate) => candidate.awaitingKeydown);
        if (earlyInput) {
          // WebKit delivered input before this printable keydown. Whether xterm
          // or our delayed fallback sent it, do not create a stale 229 baseline.
          earlyInput.awaitingKeydown = false;
        } else if (pending229Baseline === undefined) {
          pending229Baseline = term.textarea?.value ?? "";
          // The handler runs before xterm's CompositionHelper. Queue our
          // fallback after the current event so xterm's own timer gets first
          // chance to consume the change and clear the baseline.
          queueMicrotask(schedulePending229Flush);
        }
      }
    } else if (ev.type === "keyup") {
      schedulePending229Flush();
    }
    return true;
  });
  term.textarea?.addEventListener("compositionstart", () => {
    imeComposing = true;
    pending229Baseline = undefined;
    suppressNext229AfterComposition = false;
    if (pending229FlushTimer !== undefined) {
      clearTimeout(pending229FlushTimer);
      pending229FlushTimer = undefined;
    }
  });
  term.textarea?.addEventListener("compositionend", () => {
    imeComposing = false;
    pending229Baseline = undefined;
    suppressNext229AfterComposition = true;
    if (pending229FlushTimer !== undefined) {
      clearTimeout(pending229FlushTimer);
      pending229FlushTimer = undefined;
    }
    window.setTimeout(() => {
      suppressNext229AfterComposition = false;
    }, 100);
  });
  term.textarea?.addEventListener("input", (ev) => {
    const input = ev as InputEvent;
    if (
      !imeComposing &&
      input.inputType === "insertText" &&
      input.data &&
      !input.defaultPrevented &&
      modifierKeysDown.size > 0 &&
      pending229Baseline === undefined
    ) {
      queueEarlyImeInput(input.data);
    } else if (pending229Baseline !== undefined && !imeComposing) {
      schedulePending229Flush();
    }
  });

  term.onData((data) => {
    lastOnData = {
      data,
      textareaLength: term.textarea?.value.length ?? -1,
      at: performance.now(),
    };
    const earlyInput = earlyImeInputs.find(
      (candidate) => !candidate.consumed && candidate.data === data,
    );
    if (earlyInput) {
      earlyInput.consumed = true;
    }
    if (pending229Baseline !== undefined && !imeComposing) {
      const value = term.textarea?.value ?? "";
      if (data === textareaDelta(pending229Baseline, value))
        pending229Baseline = undefined;
    }
    if (!tab.id.startsWith("failed-"))
      invoke("write_session", { id: tab.id, data }).catch(() => {});
  });
  term.onTitleChange((title) => {
    const tt = title.trim();
    if (!tt) return;
    tab.title = tt;
    refreshTabEl(tab);
    if (activeId === tab.id) renderStatusbar();
  });
  // Private OSC 9780 ("<cols>;<rows>") carries the negotiated child PTY grid
  // from the CLI (Windows pipe bridge only). A positive grid means remote mode:
  // pin the terminal to it and letterbox the extra window space so the desktop
  // matches the phone. "0;0" means local (Ctrl-G) mode: unpin and fit the full
  // desktop window, since the child is then sized to the whole window.
  term.parser.registerOscHandler(9780, (data) => {
    const [c, r] = data.split(";").map((n) => parseInt(n, 10));
    if (Number.isFinite(c) && Number.isFinite(r) && c > 0 && r > 0)
      applyRemoteGrid(tab, c, r);
    else clearRemoteGrid(tab);
    return true;
  });

  try {
    const info = await invoke<SessionInfo>("create_session", {
      opts: {
        id,
        tool,
        project: project || null,
        relay: relay || null,
        rows: term.rows,
        cols: term.cols,
      },
    });
    tab.title = info.title;
    tab.mode = info.mode;
    tab.state = info.mode === "relay" ? "wait" : "local";
    tab.logPath = info.log_path || undefined;
    refreshTabEl(tab);
    renderStatusbar();
  } catch (e) {
    term.writeln(`\x1b[31m${t("launch_failed", String(e))}\x1b[0m`);
    tab.state = "exited";
    tab.id = `failed-${Date.now()}`;
    refreshTabEl(tab);
    // For relay sessions, show the friendly error panel rather than leaving
    // the raw failure only in the (hidden) terminal.
    if (relay) {
      openOverlay("ov-pair");
      showPairingError(tab, "tool");
    }
    return;
  }

  if (relay) openPairingOverlay(tab);
}

function reopenTab(tab: Tab) {
  const { tool, project, relay } = tab;
  void closeTab(tab.id, true);
  void createTab(tool, project, relay);
}

function buildTabEl(tab: Tab) {
  const tabEl = tab.tabEl;
  tabEl.innerHTML = "";
  tabEl.draggable = true;
  const st = el("span", `st ${tab.state}`);
  const name = el("span", "tname");
  name.textContent = labelForTab(tab);
  const reopen = el("span", "reopen") as HTMLSpanElement;
  reopen.textContent = "↻";
  reopen.title = t("reopen");
  reopen.onclick = (ev) => {
    ev.stopPropagation();
    reopenTab(tab);
  };
  const x = el("span", "x");
  x.textContent = "✕";
  x.onclick = (ev) => {
    ev.stopPropagation();
    closeTab(tab.id);
  };
  tabEl.append(st, name, reopen, x);
  tabEl.onclick = () => selectTab(tab.id);
  tabEl.onauxclick = (ev) => {
    if (ev.button === 1) closeTab(tab.id);
  };
  tabEl.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    selectTab(tab.id);
    openTabMenu(tab, ev.clientX, ev.clientY);
  };
  wireTabDrag(tab);
  const newtab = $("#btn-newtab");
  $("#tabbar").insertBefore(tabEl, newtab);
  scheduleTabScrollUpdate();
}

function updateTabScrollControls() {
  const tabbar = $("#tabbar");
  const state = tabScrollState(tabbar);
  const left = $("#btn-tab-scroll-left") as HTMLButtonElement;
  const right = $("#btn-tab-scroll-right") as HTMLButtonElement;
  left.hidden = !state.overflow;
  right.hidden = !state.overflow;
  left.disabled = !state.canScrollLeft;
  right.disabled = !state.canScrollRight;
}

let tabScrollUpdateQueued = false;

function scheduleTabScrollUpdate() {
  if (tabScrollUpdateQueued) return;
  tabScrollUpdateQueued = true;
  requestAnimationFrame(() => {
    tabScrollUpdateQueued = false;
    updateTabScrollControls();
  });
}

function scrollTabs(direction: -1 | 1) {
  const tabbar = $("#tabbar");
  tabbar.scrollBy({
    left: direction * Math.max(160, Math.floor(tabbar.clientWidth * 0.7)),
    behavior: "smooth",
  });
}

// The CLI bakes a work-mode hint into the terminal title, e.g.
// "myproject 【Remote/App on】Ctrl+G to toggle". Split it into the plain
// session name (shown in tabs / status bar) and the mode hint (shown in the
// top mode bar) so the narrow tab labels aren't dominated by the hint.
function parseTitle(raw: string): {
  base: string;
  modeLabel?: string;
  toggleHint?: string;
} {
  const m = raw.match(/^(.*?)\s*【([^】]*)】\s*(.*)$/);
  if (m) {
    return {
      base: m[1].trim() || raw.trim(),
      modeLabel: m[2].trim(),
      toggleHint: m[3].trim() || undefined,
    };
  }
  return { base: raw };
}

function labelForTab(tab: Tab): string {
  const suffix =
    tab.state === "exited"
      ? ` · ${t("st_exited")}`
      : tab.state === "wait"
        ? ` · ${t("st_wait")}`
        : tab.state === "syncing"
          ? ` · ${t("st_syncing")}`
          : tab.state === "paired"
            ? ` · ${t("st_paired")}`
            : "";
  return `${parseTitle(tab.title).base}${suffix}`;
}

function refreshTabEl(tab: Tab) {
  const st = tab.tabEl.querySelector(".st") as HTMLElement;
  const name = tab.tabEl.querySelector(".tname") as HTMLElement;
  if (st) st.className = `st ${tab.state}`;
  if (name) name.textContent = labelForTab(tab);
  tab.tabEl.classList.toggle("exited", tab.state === "exited");
}

/* --------------------------- tab context menu ---------------------------- */

function closeTabMenu() {
  if (tabMenuEl) {
    tabMenuEl.remove();
    tabMenuEl = null;
  }
}

interface MenuItemOpts {
  icon: string;
  label: string;
  title?: string;
  disabled?: boolean;
  danger?: boolean;
  onClick?: () => void;
}

function appendMenuItem(menu: HTMLDivElement, opts: MenuItemOpts) {
  const item = el("div", "ci") as HTMLDivElement;
  if (opts.danger) item.classList.add("danger");
  if (opts.disabled) item.classList.add("disabled");
  if (opts.title) item.title = opts.title;
  const ic = el("span", "ci-ic");
  ic.textContent = opts.icon;
  const label = el("span");
  label.textContent = opts.label;
  item.append(ic, label);
  if (!opts.disabled && opts.onClick) {
    item.onclick = (ev) => {
      ev.stopPropagation();
      closeTabMenu();
      opts.onClick!();
    };
  }
  menu.appendChild(item);
}

function openTabMenu(tab: Tab, x: number, y: number) {
  closeTabMenu();
  const menu = el("div", "ctxmenu") as HTMLDivElement;
  const isRelay = tab.mode === "relay";

  appendMenuItem(menu, {
    icon: "▣",
    label: t("view_qr"),
    title: isRelay ? undefined : t("ctx_qr_local"),
    disabled: !isRelay,
    onClick: () => openPairingOverlay(tab),
  });
  appendMenuItem(menu, {
    icon: "⧉",
    label: t("ctx_copy_link"),
    disabled: !isRelay || !tab.pairUrl,
    onClick: () => {
      if (tab.pairUrl) void navigator.clipboard?.writeText(tab.pairUrl);
    },
  });

  menu.appendChild(el("div", "ci-sep"));

  appendMenuItem(menu, {
    icon: "↻",
    label: t("reopen"),
    onClick: () => reopenTab(tab),
  });
  appendMenuItem(menu, {
    icon: "✕",
    label: t("ctx_close_tab"),
    danger: true,
    onClick: () => closeTab(tab.id),
  });

  placeMenu(menu, x, y);
}

// Append a menu to the body and clamp it inside the viewport so it never
// spills off-screen.
function placeMenu(menu: HTMLDivElement, x: number, y: number) {
  document.body.appendChild(menu);
  tabMenuEl = menu;
  const rect = menu.getBoundingClientRect();
  const left = Math.max(8, Math.min(x, window.innerWidth - rect.width - 8));
  const top = Math.max(8, Math.min(y, window.innerHeight - rect.height - 8));
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
  menu.classList.add("show");
}

// Right-click menu for the pairing QR: copy the QR as an image, or copy the
// raw pairing link as text.
function openQrMenu(qrbox: HTMLElement, x: number, y: number) {
  closeTabMenu();
  const menu = el("div", "ctxmenu") as HTMLDivElement;
  appendMenuItem(menu, {
    icon: "▣",
    label: t("ctx_copy_qr"),
    onClick: () => void copyQrImage(qrbox),
  });
  appendMenuItem(menu, {
    icon: "⧉",
    label: t("ctx_copy_link"),
    onClick: () => {
      const url = $("#pair-url").textContent || "";
      if (url) void navigator.clipboard?.writeText(url);
    },
  });
  placeMenu(menu, x, y);
}

// Rasterize the pairing QR (SVG + centered logo) onto a canvas and put a PNG on
// the clipboard, so the user can paste the QR image straight into a chat.
async function copyQrImage(qrbox: HTMLElement) {
  const svgEl = qrbox.querySelector("svg");
  if (!svgEl) return;
  const size = 512;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const loadImg = (src: string) =>
    new Promise<HTMLImageElement>((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = reject;
      img.src = src;
    });
  try {
    const svgUrl =
      "data:image/svg+xml;charset=utf-8," +
      encodeURIComponent(new XMLSerializer().serializeToString(svgEl));
    const qrImg = await loadImg(svgUrl);
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, size, size);
    const pad = Math.round(size * 0.06);
    ctx.drawImage(qrImg, pad, pad, size - pad * 2, size - pad * 2);
    try {
      const logo = await loadImg("/logo.png");
      const ls = Math.round(size * 0.2);
      const lo = Math.round((size - ls) / 2);
      const r = Math.round(ls * 0.22);
      ctx.fillStyle = "#fff";
      roundRect(ctx, lo - 6, lo - 6, ls + 12, ls + 12, r);
      ctx.fill();
      ctx.drawImage(logo, lo, lo, ls, ls);
    } catch {
      /* logo is optional */
    }
    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/png"),
    );
    if (!blob) return;
    // `navigator.clipboard.write` can't reliably put images on the OS clipboard
    // in the Tauri webview (e.g. WebKitGTK), so hand the PNG bytes to the
    // backend's clipboard plugin instead.
    const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
    await invoke("copy_image_png", { bytes });
  } catch (e) {
    console.error("copy QR failed", e);
  }
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

// Right-click menu for the terminal surface: text actions (copy / paste /
// select-all), clear, and a split toggle. Paste and copy go through the
// session so they behave the same in local and relay modes.
function openTermMenu(tab: Tab, x: number, y: number) {
  closeTabMenu();
  const menu = el("div", "ctxmenu") as HTMLDivElement;
  const hasSel = tab.term.hasSelection();

  appendMenuItem(menu, {
    icon: "⧉",
    label: t("ctx_copy"),
    disabled: !hasSel,
    onClick: () => {
      const sel = tab.term.getSelection();
      if (sel) void navigator.clipboard?.writeText(sel);
    },
  });
  appendMenuItem(menu, {
    icon: "⎘",
    label: t("ctx_paste"),
    disabled: tab.id.startsWith("failed-"),
    onClick: () => {
      void navigator.clipboard?.readText().then((text) => {
        if (text) invoke("write_session", { id: tab.id, data: text }).catch(() => {});
      });
    },
  });
  appendMenuItem(menu, {
    icon: "▦",
    label: t("ctx_selectall"),
    onClick: () => {
      tab.term.focus();
      tab.term.selectAll();
    },
  });

  menu.appendChild(el("div", "ci-sep"));

  appendMenuItem(menu, {
    icon: "⌫",
    label: t("ctx_clear"),
    onClick: () => {
      tab.term.clear();
      tab.term.focus();
    },
  });
  appendMenuItem(menu, {
    icon: "▥",
    label: t("ctx_split"),
    disabled: tabs.length < 2,
    onClick: () => toggleSplit(),
  });

  placeMenu(menu, x, y);
}

/* ----------------------------- drag reorder ------------------------------ */

let dragId: string | null = null;

function wireTabDrag(tab: Tab) {
  const elx = tab.tabEl;
  elx.addEventListener("dragstart", () => {
    dragId = tab.id;
    elx.classList.add("dragging");
  });
  elx.addEventListener("dragend", () => {
    dragId = null;
    elx.classList.remove("dragging");
  });
  elx.addEventListener("dragover", (ev) => ev.preventDefault());
  elx.addEventListener("drop", (ev) => {
    ev.preventDefault();
    if (dragId && dragId !== tab.id) moveTab(dragId, tab.id);
  });
}

function moveTab(fromId: string, toId: string) {
  const from = tabs.findIndex((t) => t.id === fromId);
  const to = tabs.findIndex((t) => t.id === toId);
  if (from < 0 || to < 0) return;
  const [moved] = tabs.splice(from, 1);
  tabs.splice(to, 0, moved);
  const newtab = $("#btn-newtab");
  const bar = $("#tabbar");
  for (const tb of tabs) bar.insertBefore(tb.tabEl, newtab);
  scheduleTabScrollUpdate();
}

/* ------------------------------ layout/split ----------------------------- */

// Split view shows two panes side by side: the focused session (activeId) and
// a secondary one (splitId). Left/right sides are assigned by tab order so the
// panes stay put when focus moves; only the highlight (.focused) shifts.
function applyLayout() {
  const stage = $("#terminals");
  const valid =
    splitOn &&
    splitId !== null &&
    splitId !== activeId &&
    tabs.some((t) => t.id === splitId) &&
    tabs.some((t) => t.id === activeId);
  stage.classList.toggle("split", valid);
  let leftId: string | null = null;
  if (valid) {
    const ai = tabs.findIndex((t) => t.id === activeId);
    const bi = tabs.findIndex((t) => t.id === splitId);
    leftId = ai <= bi ? activeId : splitId;
  }
  for (const tab of tabs) {
    const isActive = tab.id === activeId;
    const inSplit = valid && (tab.id === activeId || tab.id === splitId);
    tab.pane.classList.toggle("active", isActive);
    tab.pane.classList.toggle("split-left", inSplit && tab.id === leftId);
    tab.pane.classList.toggle("split-right", inSplit && tab.id !== leftId);
    tab.pane.classList.toggle("focused", valid && isActive);
    tab.tabEl.classList.toggle("active", isActive || (inSplit && tab.id === splitId));
    if (isActive || inSplit) {
      refitTab(tab);
    }
  }
}

function toggleSplit() {
  if (!splitOn) {
    const i = tabs.findIndex((t) => t.id === activeId);
    if (i < 0) return;
    const other = tabs[i + 1] || tabs[i - 1]; // prefer the neighbouring tab
    if (!other) return;
    splitId = other.id;
    splitOn = true;
  } else {
    splitOn = false;
    splitId = null;
  }
  applyLayout();
  renderStatusbar();
}

function selectTab(id: string) {
  // Clicking the secondary pane's tab just moves focus there; swapping the two
  // ids keeps each pane on its current side (sides follow tab order).
  if (splitOn && id === splitId && activeId) splitId = activeId;
  activeId = id;
  applyLayout();
  const tab = tabs.find((t) => t.id === id);
  if (tab) {
    tab.tabEl.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
    tab.term.focus();
  }
  renderStatusbar();
  scheduleTabScrollUpdate();
}

async function closeTab(id: string, force = false) {
  let idx = tabs.findIndex((t) => t.id === id);
  if (idx < 0) return;
  const tab = tabs[idx];
  if (!force && tab.state !== "exited" && !tab.id.startsWith("failed-")) {
    const ok = await confirmDialog({
      title: t("close_confirm_title"),
      message: t("close_confirm"),
      okLabel: t("close"),
      danger: true,
    });
    if (!ok) return;
    // The tab list may have shifted while the dialog was open.
    idx = tabs.findIndex((t) => t.id === id);
    if (idx < 0) return;
  }
  tabs.splice(idx, 1);
  if (tab.id && !tab.id.startsWith("failed-"))
    invoke("close_session", { id: tab.id }).catch(() => {});
  tab.webgl?.dispose();
  tab.term.dispose();
  tab.pane.remove();
  tab.tabEl.remove();
  if (splitId === id) {
    splitId = null;
    splitOn = false;
  }
  if (activeId === id) {
    const next = tabs[idx] || tabs[idx - 1];
    if (next) {
      // Can't split a session with itself: drop split if it would collide.
      if (splitOn && next.id === splitId) {
        splitOn = false;
        splitId = null;
      }
      selectTab(next.id);
    } else {
      activeId = null;
      applyLayout();
    }
  } else {
    applyLayout();
  }
  renderEmpty();
  renderStatusbar();
  scheduleTabScrollUpdate();
}

function runningTabs(): number {
  return tabs.filter((t) => t.state !== "exited" && !t.id.startsWith("failed-")).length;
}

function sendResize(tab: Tab) {
  if (!tab.id || tab.id.startsWith("failed-")) return;
  invoke("resize_session", {
    id: tab.id,
    rows: tab.term.rows,
    cols: tab.term.cols,
  }).catch(() => {});
}

// Report a size to the CLI as the "host" (desktop window) size without
// resizing the terminal grid. Used when the grid is pinned to a negotiated
// size but the CLI still needs to know the live window size to re-clamp.
function reportHostSize(tab: Tab, cols: number, rows: number) {
  if (!tab.id || tab.id.startsWith("failed-") || cols <= 0 || rows <= 0) return;
  invoke("resize_session", { id: tab.id, rows, cols }).catch(() => {});
}

// Fit a tab to its pane. When the CLI has negotiated a grid (remoteGrid), the
// terminal is pinned to that size and top-left aligned (letterbox); the true
// window size is still reported to the CLI so a window resize re-clamps
// min(app,host). Otherwise the terminal simply fills the pane, as it always has.
function refitTab(tab: Tab) {
  if (tab.remoteGrid) {
    const dims = tab.fit.proposeDimensions();
    if (dims && dims.cols > 0 && dims.rows > 0)
      reportHostSize(tab, dims.cols, dims.rows);
    if (
      tab.term.cols !== tab.remoteGrid.cols ||
      tab.term.rows !== tab.remoteGrid.rows
    )
      tab.term.resize(tab.remoteGrid.cols, tab.remoteGrid.rows);
    updateLetterbox(tab);
  } else {
    tab.fit.fit();
    sendResize(tab);
  }
}

// Pin the terminal to the CLI-negotiated grid and letterbox the extra window
// space (top-left aligned) so the desktop renders the same layout as the phone.
function applyRemoteGrid(tab: Tab, cols: number, rows: number) {
  tab.remoteGrid = { cols, rows };
  if (tab.term.cols !== cols || tab.term.rows !== rows) tab.term.resize(cols, rows);
  updateLetterbox(tab);
}

// Drop the pinned grid and fit the terminal to the full window again (local /
// Ctrl-G mode, where the CLI sizes the child to the whole desktop window).
function clearRemoteGrid(tab: Tab) {
  if (!tab.remoteGrid) return;
  tab.remoteGrid = undefined;
  updateLetterbox(tab);
  refitTab(tab);
}

// Toggle the letterbox layout on a pane whose terminal is pinned smaller than
// the available window space (top-left aligned, blank margin around it).
function updateLetterbox(tab: Tab) {
  tab.pane.classList.toggle("letterbox", !!tab.remoteGrid);
}

/* ------------------------------ status bar ------------------------------- */

// The active session's work-mode hint (e.g. Remote/App on + Ctrl+G to toggle)
// shown inline in the bottom status bar, just before the project path. Kept
// out of the cramped tab labels; rebuilt on every status refresh / tab switch.
function modeStatusEl(info: {
  modeLabel?: string;
  toggleHint?: string;
}): HTMLElement | null {
  if (!info.modeLabel) return null;
  const span = el("span", "statusmode");
  const on = /\bon\b|开/.test(info.modeLabel);
  const dot = el("span", `mode-dot${on ? " on" : ""}`);
  const label = el("span", "mode-label");
  // The CLI localizes the baked-in title by the OS locale, which may not match
  // the language selected in the GUI; re-translate the known labels so the
  // status bar follows the GUI language.
  label.textContent = `【${t(on ? "mode_remote_on" : "mode_local_off")}】`;
  span.append(dot, label);
  if (info.toggleHint) {
    const hint = el("span", "mode-hint");
    hint.textContent = t("mode_toggle_hint");
    span.appendChild(hint);
  }
  return span;
}

// Splitting needs at least two sessions; disable the top-bar button otherwise.
function syncSplitBtn() {
  ($("#btn-split") as HTMLButtonElement).disabled = tabs.length < 2;
}

function renderStatusbar() {
  syncSplitBtn();
  const bar = $("#statusbar");
  const tab = tabs.find((t) => t.id === activeId);
  bar.innerHTML = "";
  if (!tab) {
    const muted = el("span", "muted");
    muted.textContent = t("ready");
    bar.appendChild(muted);
    return;
  }
  const pieces: HTMLElement[] = [];
  if (tab.mode === "relay") {
    const pill = el("span", "relaypill");
    const dot = el("span", "gdot");
    if (tab.state !== "paired") dot.style.background = "var(--yellow)";
    const txt = el("span");
    txt.textContent =
      tab.state === "paired"
        ? t("paired_devices", Math.max(1, tab.peers))
        : tab.state === "syncing"
          ? t("st_syncing")
          : t("st_wait");
    const qrIcon = el("span", "relaypill-qr");
    qrIcon.innerHTML =
      '<svg viewBox="0 0 24 24" width="12" height="12" fill="currentColor" aria-hidden="true"><path d="M3 3h8v8H3V3zm2 2v4h4V5H5zm8-2h8v8h-8V3zm2 2v4h4V5h-4zM3 13h8v8H3v-8zm2 2v4h4v-4H5zm13-2h3v2h-3v-2zm-5 0h3v3h-2v-1h-1v-2zm5 5h3v3h-3v-3zm-5 0h3v3h-3v-3z"/></svg>';
    pill.append(dot, txt, qrIcon);
    pill.title = t("view_qr");
    pill.onclick = () => openPairingOverlay(tab);
    pieces.push(pill);
    pieces.push(textSpan(tab.relay));
  } else {
    pieces.push(textSpan(t("local_mode")));
  }
  const info = parseTitle(tab.title);
  pieces.push(textSpan(info.base));
  const mode = modeStatusEl(info);
  if (mode) pieces.push(mode);
  pieces.push(textSpan(tab.project));
  if (splitOn && splitId) pieces.push(textSpan(t("split_on")));
  bar.append(...withSeparators(pieces));
  const right = el("span", "right");
  right.textContent = `${tab.term.cols} × ${tab.term.rows}`;
  bar.appendChild(right);
}

function textSpan(text: string): HTMLElement {
  const s = el("span");
  s.textContent = text;
  return s;
}

function withSeparators(items: HTMLElement[]): HTMLElement[] {
  const out: HTMLElement[] = [];
  items.forEach((item, i) => {
    if (i > 0) {
      const sep = el("span", "sep");
      sep.textContent = "·";
      out.push(sep);
    }
    out.push(item);
  });
  return out;
}

/* ------------------------------ empty state ------------------------------ */

function renderEmpty() {
  $("#empty").classList.toggle("show", tabs.length === 0);
}

// Recent sessions live in their own scrollable modal (opened from the landing's
// top-right "最近会话" button) so the landing itself stays a single screen.
async function openRecentOverlay() {
  await renderRecents();
  openOverlay("ov-recent");
}

async function renderRecents() {
  const recents = await invoke<Recent[]>("list_recents").catch(() => [] as Recent[]);
  const box = $("#empty-recent");
  box.innerHTML = "";
  ($("#recent-empty") as HTMLElement).style.display = recents.length ? "none" : "block";
  for (const r of recents) {
    const rc = el("div", "rc") as HTMLDivElement;
    const text = `${r.kind} · ${r.project}${r.relay ? " · " + r.relay : ""}`;
    const label = el("span", "rc-label");
    label.textContent = text;
    rc.title = text;
    rc.onclick = () => {
      closeOverlays();
      void createTab(r.kind, r.project, r.relay);
    };
    const rm = el("span", "rcx") as HTMLSpanElement;
    rm.textContent = "✕";
    rm.title = t("delete");
    rm.onclick = (ev) => {
      ev.stopPropagation();
      void invoke("forget_recent", { id: r.id }).then(renderRecents);
    };
    rc.append(label, rm);
    box.appendChild(rc);
  }
}

/* -------------------------------- pairing -------------------------------- */

function openPairingOverlay(tab: Tab) {
  pairingTabId = tab.id;
  clearPairCountdown();
  // The view (connecting / QR / error) is chosen by renderPairingOverlay from
  // the tab's current relay state; the success view only appears live on
  // pairing.
  renderPairingOverlay(tab);
  openOverlay("ov-pair");
}

// Toggle which of the pairing overlay's sub-panels is visible and keep the
// card heading in sync with it.
type PairView = "connecting" | "qr" | "error" | "success";
function setPairView(view: PairView) {
  ($("#pair-connecting") as HTMLElement).hidden = view !== "connecting";
  ($("#pair-main") as HTMLElement).hidden = view !== "qr";
  ($("#pair-error") as HTMLElement).hidden = view !== "error";
  ($("#pair-success") as HTMLElement).hidden = view !== "success";
  ($("#pair-retry") as HTMLElement).hidden = view !== "error";
  $("#pair-heading").textContent =
    view === "connecting"
      ? t("relay_connecting_title")
      : view === "error"
        ? t("relay_failed_title")
        : t("pair_title");
}

// Friendly connection-failure panel: a plain-language reason plus Retry / Close,
// shown instead of dumping the raw relay error string on the user. `reason`
// picks the message: "relay" for a failed relay connection, "tool" when the
// selected tool itself failed to launch.
function showPairingError(tab: Tab, reason: "relay" | "tool" = "relay") {
  pairingTabId = tab.id;
  clearPairCountdown();
  $("#pair-error-log").textContent =
    reason === "tool" ? t("relay_failed_tool") : t("relay_failed_log", tab.relay);
  setPairView("error");
}

// Replace the QR panel with a "paired" confirmation that auto-closes after a
// short countdown, so a successful scan gives clear feedback and gets out of
// the way on its own.
function showPairingSuccess(tab: Tab) {
  setPairView("success");
  let remain = 3;
  const cd = $("#pair-countdown");
  cd.textContent = t("pair_success_countdown", remain);
  clearPairCountdown();
  pairCountdownTimer = window.setInterval(() => {
    remain -= 1;
    if (remain <= 0) {
      clearPairCountdown();
      if (pairingTabId === tab.id) closeOverlays();
      return;
    }
    cd.textContent = t("pair_success_countdown", remain);
  }, 1000);
}

async function renderPairingOverlay(tab: Tab) {
  const copyBtn = $("#pair-copy") as HTMLButtonElement;
  // No pairing URL yet: the relay child is still connecting. Show the progress
  // bar + "connecting to relay" log instead of a blank QR placeholder.
  if (!tab.pairUrl) {
    if (tab.state === "exited") {
      showPairingError(tab);
    } else {
      setPairView("connecting");
    }
    copyBtn.onclick = null;
    return;
  }
  setPairView("qr");
  const url = tab.pairUrl;
  try {
    const svg = await invoke<string>("render_qr", { data: url });
    $("#qr-svg").innerHTML = svg;
  } catch (e) {
    console.error("render_qr failed", e);
  }
  const qrbox = $("#qrbox") as HTMLElement;
  qrbox.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    openQrMenu(qrbox, ev.clientX, ev.clientY);
  };
  $("#pair-url").textContent = url;
  copyBtn.onclick = () => navigator.clipboard?.writeText(url);
  $("#pair-status").innerHTML =
    tab.state === "paired"
      ? `<span class="gdot"></span> ${t("st_paired")}`
      : `<span class="spinner"></span> ${t("pair_waiting")}`;
}

/* ----------------------------- diagnostics ------------------------------- */

async function openDiagnostics() {
  const info = await invoke<Diag>("diagnostics").catch(() => null);
  const box = $("#diag-info");
  box.innerHTML = "";
  if (info) {
    const rows: [string, string][] = [
      [t("diag_engine"), info.relay_engine],
      [t("diag_version"), info.cli_version || "—"],
      [t("diag_exe"), info.gui_exe],
      [t("diag_config"), info.config_path],
      [t("diag_recent"), info.recent_path],
    ];
    for (const [k, v] of rows) {
      const row = el("div", "diag-row");
      const key = el("span", "dk");
      key.textContent = k;
      const val = el("span", "dv");
      val.textContent = v;
      row.append(key, val);
      box.appendChild(row);
    }
  }
  openOverlay("ov-diag");
  await refreshDiagLog();
  if (diagTimer === null) diagTimer = window.setInterval(refreshDiagLog, 1500);
}

async function refreshDiagLog() {
  const tab = tabs.find((t) => t.id === activeId);
  const pre = $("#diag-log") as HTMLPreElement;
  if (!tab || tab.mode !== "relay" || !tab.logPath) {
    pre.textContent = t("diag_no_log");
    return;
  }
  const text = await invoke<string>("read_log_tail", {
    path: tab.logPath,
    maxBytes: 8192,
  }).catch(() => "");
  pre.textContent = text || t("diag_log_empty");
  pre.scrollTop = pre.scrollHeight;
}

/* --------------------------- shortcuts / tray ---------------------------- */

const TRAY_KEY = "relaycat.minimizeToTray";
const trayEnabled = () => localStorage.getItem(TRAY_KEY) === "1";

const SHORTCUTS: [string, string][] = [
  ["Ctrl + T", "sc_new"],
  ["Ctrl + W", "sc_close"],
  ["Ctrl + 1…9", "sc_switch"],
  ["Ctrl + \\", "sc_split"],
  ["Ctrl + ,", "sc_settings"],
  ["Ctrl + /", "sc_shortcuts"],
  ["Ctrl + G", "sc_mode"],
  ["Esc", "sc_esc"],
];

function openLicenses() {
  $("#lic-text").textContent = thirdPartyLicenses;
  openOverlay("ov-licenses");
}

function openShortcuts() {
  const list = $("#sc-list");
  list.innerHTML = "";
  for (const [keys, descKey] of SHORTCUTS) {
    const row = el("div", "sc-row");
    const k = el("kbd", "sc-keys");
    k.textContent = keys;
    const d = el("span", "sc-desc");
    d.textContent = t(descKey);
    row.append(k, d);
    list.appendChild(row);
  }
  openOverlay("ov-shortcuts");
}

/* ----------------------- paired devices / clear data --------------------- */

type PairedDevice = {
  id: string;
  kind: string;
  project: string;
  relay: string;
  paired: boolean;
  created_at: number;
  expired: boolean;
  last_used_at: number;
  use_count: number;
};

type ClearReport = {
  pairings_removed: number;
  logs_removed: number;
  recents_removed: number;
  crash_log_removed: boolean;
};

function fmtDate(unix: number): string {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleDateString();
}

async function openDevices() {
  openOverlay("ov-devices");
  await pruneExpiredPairings();
  await refreshDevices();
}

// Expired pairings can't be reused (a re-scan is required anyway), so silently
// drop them when the dialog opens — there's no value in surfacing them.
async function pruneExpiredPairings() {
  const devices = await invoke<PairedDevice[]>("list_paired_devices").catch(
    () => [] as PairedDevice[],
  );
  for (const d of devices.filter((d) => d.expired)) {
    await invoke("revoke_pairing", { project: d.project, kind: d.kind }).catch(
      () => {},
    );
  }
}

async function refreshDevices() {
  const list = $("#dev-list");
  list.innerHTML = "";
  const devices = await invoke<PairedDevice[]>("list_paired_devices").catch(
    () => [] as PairedDevice[],
  );
  if (devices.length === 0) {
    const empty = el("div", "dev-empty");
    empty.textContent = t("dev_empty");
    list.appendChild(empty);
    return;
  }
  for (const d of devices) {
    const row = el("div", "dev-row");
    const info = el("div", "dev-info");
    const title = el("div", "dev-proj");
    title.textContent = `${d.kind} · ${d.project}`;
    const meta = el("div", "dev-meta");
    const status = d.expired ? t("dev_expired") : t("dev_paired", fmtDate(d.created_at));
    meta.textContent = `${d.relay} · ${status} · ${t("dev_used", d.use_count)}`;
    info.append(title, meta);
    const btn = el("button", "btn danger sm") as HTMLButtonElement;
    btn.textContent = t("dev_revoke");
    btn.onclick = async () => {
      if (!window.confirm(t("dev_revoke_confirm"))) return;
      btn.disabled = true;
      btn.textContent = t("dev_revoking");
      try {
        await invoke("revoke_pairing", { project: d.project, kind: d.kind });
      } finally {
        await refreshDevices();
      }
    };
    row.append(info, btn);
    list.appendChild(row);
  }
}

async function clearSensitiveData() {
  const status = $("#clr-status");
  const btn = $("#btn-clear-confirm") as HTMLButtonElement;
  const opts = {
    pairings: ($("#clr-pairings") as HTMLInputElement).checked,
    recents: ($("#clr-recents") as HTMLInputElement).checked,
    logs: ($("#clr-logs") as HTMLInputElement).checked,
    crash_log: ($("#clr-crash") as HTMLInputElement).checked,
  };
  if (!opts.pairings && !opts.recents && !opts.logs && !opts.crash_log) {
    status.textContent = t("clr_none");
    return;
  }
  btn.disabled = true;
  status.textContent = t("clr_clearing");
  try {
    const r = await invoke<ClearReport>("clear_sensitive_data", { opts });
    status.textContent = t(
      "clr_done",
      r.pairings_removed,
      r.logs_removed,
      r.recents_removed,
      r.crash_log_removed ? t("clr_yes") : t("clr_no"),
    );
  } catch {
    status.textContent = t("diag_export_failed");
  } finally {
    btn.disabled = false;
  }
}

async function exportDiagnostics() {
  const status = $("#diag-export-status");
  const btn = $("#btn-diag-export") as HTMLButtonElement;
  status.textContent = t("diag_exporting");
  btn.disabled = true;
  try {
    const tab = tabs.find((t) => t.id === activeId);
    const logPath = tab && tab.mode === "relay" ? (tab.logPath ?? null) : null;
    const report = await invoke<string>("build_diagnostics_report", {
      appVersion: await getVersion(),
      generatedAt: new Date().toISOString(),
      logPath,
    });
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const dest = await saveDialog({
      defaultPath: `relaycat-diagnostics-${stamp}.txt`,
      filters: [{ name: "Text", extensions: ["txt"] }],
    });
    if (!dest) {
      status.textContent = "";
      return;
    }
    await invoke("write_text_file", { path: dest, contents: report });
    status.textContent = t("diag_export_done");
  } catch {
    status.textContent = t("diag_export_failed");
  } finally {
    btn.disabled = false;
  }
}

/* ------------------------------- settings -------------------------------- */

// Settings are split into grouped tabs (general / appearance / tools / data)
// so a single page never shows too much at once.
function showSettingsGroup(group: string) {
  document.querySelectorAll<HTMLElement>("#ov-settings .set-group").forEach((g) => {
    g.classList.toggle("active", g.dataset.group === group);
  });
  document.querySelectorAll<HTMLElement>("#set-tabs .set-tab").forEach((b) => {
    b.classList.toggle("active", b.dataset.group === group);
  });
}

async function openSettings() {
  config = await invoke<ConfigDto>("get_config");
  tools = await invoke<Tool[]>("list_tools");
  showSettingsGroup("general");

  const toolSel = $("#set-default-tool") as HTMLSelectElement;
  toolSel.innerHTML = "";
  for (const tool of tools) {
    const opt = document.createElement("option");
    opt.value = tool.name;
    opt.textContent = tool.label;
    if (tool.name === config.default_tool) opt.selected = true;
    toolSel.appendChild(opt);
  }
  // Disable tools that aren't installed so they can't be set as the default
  // (a default that can't launch would just fail every new session).
  void refreshSettingsToolOptions();
  ($("#set-default-relay") as HTMLInputElement).value = config.default_relay || "";
  ($("#set-return") as HTMLInputElement).checked = config.return_to_launcher;
  ($("#set-fontsize") as HTMLSelectElement).value = String(fontSize());
  ($("#set-lang") as HTMLSelectElement).value = lang;
  ($("#set-theme") as HTMLSelectElement).value = theme;
  ($("#set-tray") as HTMLInputElement).checked = trayEnabled();
  ($("#set-autostart") as HTMLInputElement).checked = await isAutostartEnabled().catch(
    () => false,
  );

  const toolList = $("#set-tools");
  toolList.innerHTML = "";
  if (config.tools.length === 0) {
    const note = el("div", "empty-note");
    note.textContent = t("no_custom_tools");
    toolList.appendChild(note);
  }
  for (const tool of config.tools) {
    const ti = el("div", "ti");
    const nm = el("span", "nm");
    nm.textContent = tool.name;
    const meta = el("span", "meta");
    meta.textContent = `cmd=${tool.cmd}${tool.args.length ? "  args=" + tool.args.join(" ") : ""}`;
    const rm = el("span", "rm") as HTMLSpanElement;
    rm.textContent = "✕";
    rm.title = t("delete");
    rm.onclick = () => {
      config.tools = config.tools.filter((x) => x.name !== tool.name);
      void invoke("save_config", { config }).then(openSettings);
    };
    ti.append(nm, meta, rm);
    toolList.appendChild(ti);
  }

  const favList = $("#set-favorites");
  favList.innerHTML = "";
  if (config.favorites.length === 0) {
    const note = el("div", "empty-note");
    note.textContent = t("no_favorites");
    favList.appendChild(note);
  }
  for (const fav of config.favorites) {
    const ti = el("div", "ti");
    const star = el("span");
    star.textContent = "★";
    star.style.color = "var(--yellow)";
    const meta = el("span", "meta");
    meta.textContent = fav;
    const rm = el("span", "rm") as HTMLSpanElement;
    rm.textContent = "✕";
    rm.title = t("delete");
    rm.onclick = async () => {
      const ok = await confirmDialog({
        title: t("fav_remove_title"),
        message: t("fav_remove_confirm", fav),
        okLabel: t("delete"),
        danger: true,
      });
      if (!ok) return;
      config.favorites = config.favorites.filter((f) => f !== fav);
      void invoke("save_config", { config }).then(openSettings);
    };
    ti.append(star, meta, rm);
    favList.appendChild(ti);
  }

  ($("#set-tool-name") as HTMLInputElement).value = "";
  ($("#set-tool-cmd") as HTMLInputElement).value = "";
  ($("#set-tool-args") as HTMLInputElement).value = "";

  void renderSettingsToolDetect();
  openOverlay("ov-settings");
}

async function addCustomTool() {
  const name = ($("#set-tool-name") as HTMLInputElement).value.trim();
  const cmd = ($("#set-tool-cmd") as HTMLInputElement).value.trim();
  const argsRaw = ($("#set-tool-args") as HTMLInputElement).value.trim();
  if (!name || !cmd) {
    alert(t("tool_need_name_cmd"));
    return;
  }
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(name)) {
    alert(t("tool_bad_name"));
    return;
  }
  config = await invoke<ConfigDto>("get_config");
  if (config.tools.some((x) => x.name === name)) {
    alert(t("tool_exists", name));
    return;
  }
  const args = argsRaw ? argsRaw.split(/\s+/) : [];
  config.tools.push({ name, cmd, args });
  await invoke("save_config", { config }).catch((e) => console.error(e));
  await openSettings();
}

// Persist the in-memory config to disk (shared with the CLI).
async function persistConfig() {
  await invoke("save_config", { config }).catch((e) => console.error(e));
}

function applyFontSize(px: number) {
  localStorage.setItem("relaycat.fontSize", String(px));
  for (const tab of tabs) {
    tab.term.options.fontSize = px;
    refitTab(tab);
  }
}

// Settings apply immediately (no Save button): each control persists on change.
function wireSettingsControls() {
  ($("#set-default-tool") as HTMLSelectElement).onchange = (e) => {
    config.default_tool = (e.target as HTMLSelectElement).value || null;
    void persistConfig();
  };
  ($("#set-default-relay") as HTMLInputElement).onchange = (e) => {
    config.default_relay = (e.target as HTMLInputElement).value.trim() || null;
    void persistConfig();
  };
  ($("#set-return") as HTMLInputElement).onchange = (e) => {
    config.return_to_launcher = (e.target as HTMLInputElement).checked;
    void persistConfig();
  };
  ($("#set-lang") as HTMLSelectElement).onchange = (e) => {
    setLanguage((e.target as HTMLSelectElement).value as Lang);
  };
  ($("#set-theme") as HTMLSelectElement).onchange = (e) => {
    setTheme((e.target as HTMLSelectElement).value as Theme);
  };
  ($("#set-fontsize") as HTMLSelectElement).onchange = (e) => {
    applyFontSize(Number((e.target as HTMLSelectElement).value));
  };
  ($("#set-tray") as HTMLInputElement).onchange = (e) => {
    localStorage.setItem(TRAY_KEY, (e.target as HTMLInputElement).checked ? "1" : "0");
  };
  ($("#set-autostart") as HTMLInputElement).onchange = async (e) => {
    const want = (e.target as HTMLInputElement).checked;
    try {
      const isOn = await isAutostartEnabled();
      if (want && !isOn) await enableAutostart();
      else if (!want && isOn) await disableAutostart();
    } catch (err) {
      console.error(err);
    }
  };
}

/* -------------------------------- events --------------------------------- */

function wireEvents() {
  listen<OutputEvent>("session://output", (event) => {
    const tab = tabs.find((t) => t.id === event.payload.id);
    if (tab) tab.term.write(new Uint8Array(event.payload.data));
  });

  listen<StatusEvent>("session://status", (event) => {
    const tab = tabs.find((t) => t.id === event.payload.id);
    if (!tab) return;
    if (event.payload.state === "exited") {
      const wasConnecting = tab.mode === "relay" && tab.state !== "paired" && !tab.pairUrl;
      tab.state = "exited";
      const code = event.payload.code ?? 0;
      tab.term.writeln(`\r\n\x1b[90m${t("proc_exited", code)}\x1b[0m`);
      refreshTabEl(tab);
      renderStatusbar();
      // A relay session that exits before ever producing a pairing URL never
      // connected: surface a friendly error in the pairing overlay (in place
      // of the raw stderr error string) so the user gets Retry / Help.
      if (wasConnecting && pairingTabId === tab.id && $("#ov-pair").classList.contains("show")) {
        showPairingError(tab);
      }
    }
  });

  listen<PairingEvent>("session://pairing", (event) => {
    const tab = tabs.find((t) => t.id === event.payload.id);
    if (!tab) return;
    tab.pairUrl = event.payload.url;
    if (pairingTabId === tab.id) void renderPairingOverlay(tab);
  });

  listen<RelayEvent>("session://relay", (event) => {
    const tab = tabs.find((t) => t.id === event.payload.id);
    if (!tab || tab.state === "exited") return;
    const state = event.payload.state;
    if (state === "paired") {
      // Green only when the terminal stream is actually live, not merely
      // when the secure session was accepted.
      if (tab.state !== "paired") tab.peers = Math.max(1, tab.peers + 1);
      tab.state = "paired";
      refreshTabEl(tab);
      renderStatusbar();
      // If the QR is on screen for this tab, swap it for the auto-closing
      // success confirmation; otherwise just keep the state in sync.
      if (pairingTabId === tab.id && $("#ov-pair").classList.contains("show")) {
        showPairingSuccess(tab);
      }
    } else if (state === "syncing" || state === "wait") {
      tab.state = state;
      refreshTabEl(tab);
      renderStatusbar();
    } else if (state === "error") {
      // Stable relay error propagated from the CLI log: show the same
      // code-derived reason/retryability the mobile apps present.
      const code = event.payload.code ?? "unknown";
      const hint = event.payload.retryable ? t("relay_err_retryable") : t("relay_err_fatal");
      const detail = event.payload.message ? `: ${event.payload.message}` : "";
      tab.term.writeln(`\r\n\x1b[33m${t("relay_err_line", code, hint)}${detail}\x1b[0m`);
    }
  });
}

// Latest update found by the Tauri updater (self-update of the GUI app).
let pendingUpdate: Update | null = null;

async function checkUpdate() {
  // Launch-time silent check; surface a banner if a new version is available.
  try {
    const update = await check();
    const banner = $("#update-banner");
    if (update) {
      pendingUpdate = update;
      banner.textContent = t("update_banner", update.version);
      banner.classList.add("show");
    } else {
      banner.classList.remove("show");
    }
  } catch {
    /* offline / endpoint unreachable: silently ignore */
  }
}

/* --------------------------------- about --------------------------------- */

async function openAbout() {
  openOverlay("ov-about");
  $("#about-update-status").textContent = "";
  try {
    $("#about-app-version").textContent = "v" + (await getVersion());
  } catch {
    $("#about-app-version").textContent = "\u2014";
  }
  invoke<Diag>("diagnostics")
    .then((d) => {
      $("#about-cli-version").textContent = d.cli_version || "\u2014";
    })
    .catch(() => {});
}

async function checkUpdateInteractive() {
  const status = $("#about-update-status");
  const installBtn = $<HTMLButtonElement>("#btn-install-update");
  const notes = $("#about-update-notes");
  installBtn.hidden = true;
  notes.hidden = true;
  notes.textContent = "";
  status.textContent = t("about_checking");
  try {
    const update = await check();
    if (update) {
      pendingUpdate = update;
      status.textContent = t("about_update_found", update.version);
      const body = update.body?.trim();
      if (body) {
        notes.textContent = body;
        notes.hidden = false;
      }
      installBtn.hidden = false;
      const banner = $("#update-banner");
      banner.textContent = t("update_banner", update.version);
      banner.classList.add("show");
    } else {
      status.textContent = t("about_uptodate");
    }
  } catch {
    status.textContent = t("about_update_failed");
  }
}

async function installPendingUpdate() {
  if (!pendingUpdate) return;
  const status = $("#about-update-status");
  const installBtn = $<HTMLButtonElement>("#btn-install-update");
  installBtn.disabled = true;
  let downloaded = 0;
  let total = 0;
  try {
    await pendingUpdate.downloadAndInstall((event: DownloadEvent) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength ?? 0;
          status.textContent = t("about_downloading", 0);
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          status.textContent = t(
            "about_downloading",
            total ? Math.floor((downloaded / total) * 100) : 0,
          );
          break;
        case "Finished":
          status.textContent = t("about_installing");
          break;
      }
    });
    status.textContent = t("about_restart");
    await relaunch();
  } catch {
    status.textContent = t("about_update_failed");
    installBtn.disabled = false;
  }
}

/* ------------------------- onboarding / help ----------------------------- */

const ONBOARD_KEY = "relaycat.onboarded";
const ONBOARD_STEPS = 4;
const ONBOARD_RELAY_STEP = 2;
const ONBOARD_TOOLS_STEP = 3;
const DEFAULT_OFFICIAL_RELAY = "wss://001.relaycat.cn";
let onboardStep = 0;

type ToolStatus = {
  name: string;
  label: string;
  kind: string;
  program: string;
  installed: boolean;
  version: string | null;
};

// Install guides per builtin tool. These point at the official site; the
// pages may not exist yet (placeholder until tutorials ship).
const TOOL_GUIDE_URLS: Record<string, string> = {
  codex: "https://www.relaycat.cn/docs/tools/codex",
  claude: "https://www.relaycat.cn/docs/tools/claude-code",
  opencode: "https://www.relaycat.cn/docs/tools/opencode",
  gemini: "https://www.relaycat.cn/docs/tools/gemini",
};
const TOOL_GUIDE_FALLBACK = "https://www.relaycat.cn/docs";

function renderOnboard() {
  document.querySelectorAll<HTMLElement>("#ov-onboard .ob-step").forEach((s) => {
    s.hidden = Number(s.dataset.step) !== onboardStep;
  });
  const dots = $("#ob-dots");
  dots.innerHTML = "";
  for (let i = 0; i < ONBOARD_STEPS; i++) {
    dots.appendChild(el("span", i === onboardStep ? "ob-dot on" : "ob-dot"));
  }
  ($("#ob-back") as HTMLElement).hidden = onboardStep === 0;
  $("#ob-next").textContent =
    onboardStep === ONBOARD_STEPS - 1 ? t("ob_start") : t("ob_next");
  if (onboardStep === ONBOARD_TOOLS_STEP) void renderOnboardTools();
}

function elText(tag: string, cls: string, text: string): HTMLElement {
  const node = el(tag, cls);
  node.textContent = text;
  return node;
}

async function renderToolList(list: HTMLElement, minDurationMs = 0) {
  list.innerHTML = "";
  const progress = el("li", "ob-tools-progress");
  const bar = el("div", "pair-progress");
  bar.appendChild(el("div", "pair-progress-bar"));
  progress.appendChild(bar);
  progress.appendChild(elText("div", "ob-tools-loading", t("ob_tools_loading")));
  list.appendChild(progress);
  let tools: ToolStatus[];
  try {
    const [result] = await Promise.all([
      invoke<ToolStatus[]>("detect_tools"),
      minDurationMs > 0
        ? new Promise((r) => setTimeout(r, minDurationMs))
        : Promise.resolve(),
    ]);
    tools = result;
  } catch (e) {
    console.error(e);
    return;
  }
  list.innerHTML = "";
  for (const tool of tools) {
    const row = el("li", tool.installed ? "ob-tool ok" : "ob-tool missing");
    const left = el("div", "ob-tool-main");
    left.appendChild(elText("span", "ob-tool-name", tool.label));
    left.appendChild(elText("span", "ob-tool-cmd", tool.program));
    if (tool.installed && tool.version) {
      left.appendChild(elText("span", "ob-tool-ver", tool.version));
    }
    row.appendChild(left);
    if (tool.installed) {
      const badge = el("span", "ob-tool-badge ok");
      badge.appendChild(elText("span", "ob-tool-check", "✓"));
      badge.appendChild(elText("span", "", t("ob_tool_installed")));
      row.appendChild(badge);
    } else {
      const right = el("div", "ob-tool-right");
      right.appendChild(elText("span", "ob-tool-badge missing", t("ob_tool_missing")));
      if (tool.kind === "builtin") {
        const url = TOOL_GUIDE_URLS[tool.name] || TOOL_GUIDE_FALLBACK;
        const link = elText("a", "about-link ob-tool-guide", t("ob_tool_guide")) as HTMLAnchorElement;
        link.href = "#";
        link.onclick = (ev) => {
          ev.preventDefault();
          void invoke("open_url", { url });
        };
        right.appendChild(link);
      }
      row.appendChild(right);
    }
    list.appendChild(row);
  }

  // A live detection sweep just ran (and the backend persisted it). Refresh the
  // in-memory cache and any open pickers so the new-session chips and the
  // settings default-tool dropdown reflect the new result immediately.
  toolInstalled = statusMap(tools);
  renderToolChips();
  applyToolStatusToSettings();
}

function renderOnboardTools() {
  return renderToolList($("#ob-tools"));
}

function renderSettingsToolDetect() {
  return renderToolList($("#set-detect-list"), 2000);
}

async function openOnboarding() {
  config = await invoke<ConfigDto>("get_config");
  onboardStep = 0;
  ($("#ob-relay") as HTMLInputElement).value =
    config.default_relay || DEFAULT_OFFICIAL_RELAY;
  renderOnboard();
  openOverlay("ov-onboard");
}

// Validate + persist the relay typed on the setup step. Returns false (and
// flags the field) when empty so the wizard can't advance past it.
async function commitOnboardRelay(): Promise<boolean> {
  const relayInput = $("#ob-relay") as HTMLInputElement;
  const relay = relayInput.value.trim();
  if (!relay) {
    relayInput.classList.add("invalid");
    relayInput.title = t("ob_relay_required");
    relayInput.focus();
    return false;
  }
  relayInput.classList.remove("invalid");
  relayInput.title = "";
  if (relay !== (config.default_relay || "")) {
    const updated: ConfigDto = { ...config, default_relay: relay };
    await invoke("save_config", { config: updated }).catch((e) => console.error(e));
    config = updated;
  }
  return true;
}

async function finishOnboarding() {
  if (!(await commitOnboardRelay())) return;
  localStorage.setItem(ONBOARD_KEY, "1");
  closeOverlays();
  // First run: run (and persist) a detection sweep so the new-session picker
  // has authoritative tool availability before it opens.
  await invoke<ToolStatus[]>("detect_tools")
    .then((s) => {
      toolInstalled = statusMap(s);
    })
    .catch((e) => console.error(e));
  void openNewSession();
}

async function onboardNext() {
  // Gate progression on a valid relay before leaving the setup step.
  if (onboardStep === ONBOARD_RELAY_STEP && !(await commitOnboardRelay())) return;
  if (onboardStep < ONBOARD_STEPS - 1) {
    onboardStep++;
    renderOnboard();
  } else {
    void finishOnboarding();
  }
}

function onboardBack() {
  if (onboardStep > 0) {
    onboardStep--;
    renderOnboard();
  }
}

function skipOnboarding() {
  localStorage.setItem(ONBOARD_KEY, "1");
  closeOverlays();
  // Populate the detection cache in the background even when the wizard is
  // skipped, so the next new-session open reflects real tool availability.
  void invoke("detect_tools").catch((e) => console.error(e));
}

/* ------------------------------- tooltips -------------------------------- */

// Lightweight hover tooltips for the "?" markers next to settings rows. A
// single floating element is positioned under (or above) the hovered marker so
// the explanation never gets clipped by the overlay card's bounds.
function wireTooltips() {
  let tipEl: HTMLDivElement | null = null;
  const hide = () => {
    tipEl?.remove();
    tipEl = null;
  };
  document.querySelectorAll<HTMLElement>("[data-tip]").forEach((node) => {
    const show = () => {
      hide();
      tipEl = el("div", "tip-pop") as HTMLDivElement;
      tipEl.textContent = t(node.dataset.tip!);
      document.body.appendChild(tipEl);
      const r = node.getBoundingClientRect();
      const tr = tipEl.getBoundingClientRect();
      let left = r.left + r.width / 2 - tr.width / 2;
      left = Math.max(8, Math.min(left, window.innerWidth - tr.width - 8));
      let top = r.bottom + 8;
      if (top + tr.height > window.innerHeight - 8) top = r.top - tr.height - 8;
      tipEl.style.left = `${left}px`;
      tipEl.style.top = `${top}px`;
    };
    node.addEventListener("mouseenter", show);
    node.addEventListener("focus", show);
    node.addEventListener("mouseleave", hide);
    node.addEventListener("blur", hide);
  });
}

/* ------------------------------ coach marks ------------------------------ */

const COACH_KEY = "relaycat.coached";

type CoachStep = { target: string; title: string; body: string; place: "below" | "above" };
const COACH_STEPS: CoachStep[] = [
  { target: "#empty-new", title: "coach_new_title", body: "coach_new_body", place: "below" },
  { target: "#statusbar", title: "coach_mode_title", body: "coach_mode_body", place: "above" },
];
let coachStep = 0;

// Show the one-time first-run hints, but only on the bare landing (no tabs, no
// open overlay) so they never fight with another dialog. Persisted via
// localStorage so they appear exactly once.
function maybeShowCoachMarks() {
  if (localStorage.getItem(COACH_KEY)) return;
  if (document.querySelector(".overlay.show")) return;
  if (tabs.length !== 0) return;
  if (document.getElementById("coach")) return;
  startCoachMarks();
}

function endCoachMarks() {
  localStorage.setItem(COACH_KEY, "1");
  document.getElementById("coach")?.remove();
  window.removeEventListener("resize", positionCoach);
}

function startCoachMarks() {
  coachStep = 0;
  const root = el("div", "coach") as HTMLDivElement;
  root.id = "coach";
  root.innerHTML = `
    <div class="coach-ring" id="coach-ring"></div>
    <div class="coach-bubble" id="coach-bubble">
      <button class="coach-skip" id="coach-skip" aria-label="close">×</button>
      <div class="coach-title" id="coach-title"></div>
      <div class="coach-body" id="coach-body"></div>
      <div class="coach-foot">
        <span class="coach-step" id="coach-stepn"></span>
        <button class="btn primary" id="coach-next"></button>
      </div>
    </div>`;
  document.body.appendChild(root);
  root.onclick = (e) => {
    if (e.target === root) endCoachMarks();
  };
  $("#coach-skip").onclick = endCoachMarks;
  $("#coach-next").onclick = () => {
    if (coachStep < COACH_STEPS.length - 1) {
      coachStep++;
      renderCoach();
    } else {
      endCoachMarks();
    }
  };
  window.addEventListener("resize", positionCoach);
  renderCoach();
}

function renderCoach() {
  const step = COACH_STEPS[coachStep];
  $("#coach-title").textContent = t(step.title);
  $("#coach-body").textContent = t(step.body);
  $("#coach-stepn").textContent = `${coachStep + 1} / ${COACH_STEPS.length}`;
  $("#coach-next").textContent =
    coachStep < COACH_STEPS.length - 1 ? t("coach_next") : t("coach_done");
  positionCoach();
}

function positionCoach() {
  const step = COACH_STEPS[coachStep];
  const target = document.querySelector(step.target) as HTMLElement | null;
  const ring = document.getElementById("coach-ring");
  const bubble = document.getElementById("coach-bubble");
  if (!target || !ring || !bubble) return;
  const r = target.getBoundingClientRect();
  ring.style.display = "block";
  ring.style.left = `${r.left - 6}px`;
  ring.style.top = `${r.top - 6}px`;
  ring.style.width = `${r.width + 12}px`;
  ring.style.height = `${r.height + 12}px`;
  const br = bubble.getBoundingClientRect();
  let left = r.left + r.width / 2 - br.width / 2;
  left = Math.max(12, Math.min(left, window.innerWidth - br.width - 12));
  let top = step.place === "below" ? r.bottom + 14 : r.top - br.height - 14;
  top = Math.max(12, Math.min(top, window.innerHeight - br.height - 12));
  bubble.style.left = `${left}px`;
  bubble.style.top = `${top}px`;
}

/* --------------------------------- init ---------------------------------- */

function wireUi() {
  $("#btn-new").onclick = openNewSession;
  $("#btn-newtab").onclick = openNewSession;
  $("#btn-tab-scroll-left").onclick = () => scrollTabs(-1);
  $("#btn-tab-scroll-right").onclick = () => scrollTabs(1);
  $("#tabbar").addEventListener("scroll", updateTabScrollControls, { passive: true });
  new ResizeObserver(scheduleTabScrollUpdate).observe($("#tabbar"));
  scheduleTabScrollUpdate();
  $("#empty-new").onclick = openNewSession;
  $("#empty-recent-btn").onclick = openRecentOverlay;
  $("#btn-settings").onclick = openSettings;
  $("#btn-theme").onclick = toggleTheme;
  $("#btn-open-diag").onclick = openDiagnostics;
  $("#btn-diag-export").onclick = exportDiagnostics;
  $("#btn-help").onclick = () => openOverlay("ov-help");
  $("#btn-about").onclick = openAbout;
  $("#btn-open-about").onclick = openAbout;
  $("#btn-open-devices").onclick = openDevices;
  $("#btn-open-clear").onclick = () => openOverlay("ov-clear");
  $("#btn-clear-confirm").onclick = clearSensitiveData;
  $("#ob-next").onclick = onboardNext;
  $("#ob-back").onclick = onboardBack;
  $("#ob-skip").onclick = skipOnboarding;
  $("#ob-get-app").onclick = () =>
    void invoke("open_url", { url: "https://www.relaycat.cn/" });
  $("#help-guide").onclick = () => void openOnboarding();
  $("#help-docs").onclick = () =>
    void invoke("open_url", { url: "https://www.relaycat.cn/docs" });
  $("#help-feedback").onclick = () =>
    void invoke("open_url", { url: "https://www.relaycat.cn/contact" });
  $("#help-detect").onclick = async () => {
    await openSettings();
    showSettingsGroup("detect");
  };
  $("#btn-redetect").onclick = () => void renderSettingsToolDetect();
  $("#help-about").onclick = openAbout;
  $("#help-shortcuts").onclick = openShortcuts;
  $("#help-licenses").onclick = openLicenses;
  $("#btn-check-update").onclick = checkUpdateInteractive;
  $("#btn-install-update").onclick = installPendingUpdate;
  document.querySelectorAll(".about-link").forEach((a) => {
    (a as HTMLElement).onclick = (ev) => {
      ev.preventDefault();
      const url = (a as HTMLElement).dataset.url;
      if (url) void invoke("open_url", { url });
    };
  });
  // The new-session "check tools" link opens the dedicated tool-detection
  // settings page (install guides + re-detect). Returning to a fresh
  // new-session dialog re-probes install status, so newly-installed tools
  // become selectable.
  // NOTE: must be registered AFTER the generic .about-link handler above,
  // which would otherwise overwrite this onclick.
  $("#ns-tool-check").onclick = async (ev) => {
    ev.preventDefault();
    await openSettings();
    showSettingsGroup("detect");
  };
  // Same ordering constraint: #about-licenses is an .about-link without a
  // data-url, so its specific handler must win over the generic one.
  $("#about-licenses").onclick = (e) => {
    e.preventDefault();
    openLicenses();
  };
  $("#btn-split").onclick = toggleSplit;
  $("#btn-launch").onclick = launchSession;
  $("#pair-retry").onclick = () => {
    const tab = tabs.find((t) => t.id === pairingTabId);
    closeOverlays();
    if (tab) reopenTab(tab);
  };
  wireTooltips();
  ($("#relay-input") as HTMLInputElement).oninput = syncLaunchValidity;
  ($("#proj-input") as HTMLInputElement).oninput = syncLaunchValidity;
  $("#btn-browse").onclick = browseProject;
  wireSettingsControls();
  $("#btn-add-tool").onclick = addCustomTool;
  $("#update-banner").onclick = () => {
    openAbout();
    void checkUpdateInteractive();
  };
  document.querySelectorAll("[data-close]").forEach((b) => {
    (b as HTMLElement).onclick = () =>
      dismissOverlay((b as HTMLElement).closest(".overlay") as HTMLElement | null);
  });
  // Every dialog gets a top-right "×" close affordance, routed to the same
  // dismissal logic as its footer button / backdrop / Escape.
  document.querySelectorAll<HTMLElement>(".overlay > .card").forEach((card) => {
    const ov = card.closest(".overlay") as HTMLElement;
    const x = document.createElement("button");
    x.type = "button";
    x.className = "card-x";
    x.setAttribute("aria-label", t("close"));
    x.title = t("close");
    x.innerHTML =
      '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>';
    x.onclick = () => closeDialog(ov);
    card.appendChild(x);
  });
  document.querySelectorAll(".overlay").forEach((o) => {
    o.addEventListener("click", (ev) => {
      if (ev.target === o) dismissOverlay(o as HTMLElement);
    });
  });
  document.querySelectorAll("#set-tabs .set-tab").forEach((b) => {
    (b as HTMLElement).onclick = () =>
      showSettingsGroup((b as HTMLElement).dataset.group!);
  });

  // Dismiss the tab context menu on any outside interaction.
  window.addEventListener("click", () => closeTabMenu());
  window.addEventListener("blur", () => closeTabMenu());
  window.addEventListener("resize", () => closeTabMenu());
  window.addEventListener("contextmenu", (ev) => {
    closeTabMenu();
    // Suppress the webview's native context menu (reload / inspect element) on
    // pages without a custom menu. Editable fields keep their native menu so
    // copy/paste still works there. Elements with their own menu already call
    // stopPropagation, so this never runs for them.
    const target = ev.target as HTMLElement | null;
    const editable =
      !!target &&
      (target.isContentEditable ||
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA");
    if (!editable) ev.preventDefault();
  });

  window.addEventListener("keydown", (ev) => {
    const mod = ev.ctrlKey || ev.metaKey;
    if (mod && ev.key === "t") {
      ev.preventDefault();
      openNewSession();
    } else if (mod && ev.key === "w") {
      ev.preventDefault();
      if (activeId) void closeTab(activeId);
    } else if (mod && ev.key === ",") {
      ev.preventDefault();
      openSettings();
    } else if (mod && ev.key === "\\") {
      ev.preventDefault();
      toggleSplit();
    } else if (mod && ev.key === "/") {
      ev.preventDefault();
      openShortcuts();
    } else if (ev.key === "Escape") {
      dismissOverlay(document.querySelector(".overlay.show"));
      closeTabMenu();
    } else if (mod && /^[1-9]$/.test(ev.key)) {
      const idx = Number(ev.key) - 1;
      if (tabs[idx]) {
        ev.preventDefault();
        selectTab(tabs[idx].id);
      }
    }
  });

  const ro = new ResizeObserver(() => {
    for (const tab of tabs) {
      if (tab.id === activeId || (splitOn && tab.id === splitId)) {
        refitTab(tab);
      }
    }
    renderStatusbar();
  });
  ro.observe($("#terminals"));

  void getCurrentWindow().onCloseRequested(async (event) => {
    if (trayEnabled()) {
      event.preventDefault();
      await getCurrentWindow().hide();
      return;
    }
    if (runningTabs() > 0) {
      event.preventDefault();
      const ok = await confirmDialog({
        title: t("quit_confirm_title"),
        message: t("quit_confirm"),
        okLabel: t("quit_ok"),
        danger: true,
      });
      if (ok) await getCurrentWindow().destroy();
    }
  });
}

async function init() {
  // WebView2 renders fullscreen backdrop blur and dialog entrance animations
  // noticeably janky; a body class lets the stylesheet drop those effects so
  // dialogs open instantly on Windows (macOS/Linux keep them).
  if (navigator.userAgent.includes("Windows")) {
    document.body.classList.add("plat-win");
  }
  applyI18n();
  applyTheme();
  wireUi();
  wireEvents();
  await renderEmpty();
  renderStatusbar();
  void checkUpdate();
  if (!localStorage.getItem(ONBOARD_KEY)) void openOnboarding();
  else window.setTimeout(maybeShowCoachMarks, 400);
}

// Fade out the boot splash once the UI is ready, keeping it visible for at
// least 500ms (since page load) so the loading bar reads as intentional rather
// than a flash.
async function hideBootSplash() {
  const el = document.getElementById("boot-splash");
  if (!el) return;
  // WebView2 composites the fullscreen fade and the looping splash animations
  // slowly, making the splash → main UI hand-off stutter; on Windows drop the
  // minimum delay and fade and remove the splash immediately.
  if (document.body.classList.contains("plat-win")) {
    el.remove();
    return;
  }
  const wait = Math.max(0, 500 - performance.now());
  if (wait > 0) await new Promise((r) => window.setTimeout(r, wait));
  el.classList.add("boot-hide");
  window.setTimeout(() => el.remove(), 360);
}

void init().finally(() => {
  void hideBootSplash();
});
