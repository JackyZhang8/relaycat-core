import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ITheme } from "@xterm/xterm";

import {
  embeddedShellAction,
  embeddedShellHeight,
  storedEmbeddedShellHeight,
} from "./embedded-shell-model";

interface SessionInfo {
  id: string;
  title: string;
  mode: string;
}

interface OutputEvent {
  id: string;
  data: number[];
}

interface StatusEvent {
  id: string;
  state: string;
  code?: number;
}

interface EmbeddedShellTab {
  id: string;
  project: string;
  title: string;
  state: "launching" | "running" | "exited";
  ready: Promise<void>;
  closing: boolean;
  term: Terminal;
  fit: FitAddon;
  pane: HTMLDivElement;
  tabEl: HTMLButtonElement;
}

interface EmbeddedShellPanelOptions {
  translate: (key: string, ...args: (string | number)[]) => string;
  terminalOptions: () => ConstructorParameters<typeof Terminal>[0];
  currentProject: () => string | null;
  focusMainTerminal: () => void;
  afterLayoutChange: () => void;
  confirm: (message: string) => Promise<boolean>;
}

export interface EmbeddedShellPanelController {
  toggleForProject(project: string | null): Promise<void>;
  createForProject(project: string | null): Promise<void>;
  hide(): void;
  refit(): void;
  runningCount(): number;
  updateAppearance(theme: ITheme, fontSize: number): void;
}

const MAX_EMBEDDED_SHELLS = 8;
const HEIGHT_STORAGE_KEY = "relaycat.embeddedShellHeight";

export function createEmbeddedShellPanel(
  options: EmbeddedShellPanelOptions,
): EmbeddedShellPanelController {
  const { translate: t } = options;
  const host = document.querySelector("#terminal-stage") as HTMLElement;
  const panel = document.querySelector("#embedded-shell-panel") as HTMLElement;
  const tabsEl = document.querySelector("#embedded-shell-tabs") as HTMLElement;
  const terminalsEl = document.querySelector("#embedded-shell-terminals") as HTMLElement;
  const pathEl = document.querySelector("#embedded-shell-path") as HTMLElement;
  const resizer = document.querySelector("#embedded-shell-resizer") as HTMLElement;
  const addBtn = document.querySelector("#embedded-shell-add") as HTMLButtonElement;
  const hideBtn = document.querySelector("#embedded-shell-hide") as HTMLButtonElement;
  const closeAllBtn = document.querySelector("#embedded-shell-close-all") as HTMLButtonElement;

  const shells: EmbeddedShellTab[] = [];
  let activeShellId: string | null = null;
  let opened = false;
  let resizing = false;

  const storedHeight = storedEmbeddedShellHeight(
    localStorage.getItem(HEIGHT_STORAGE_KEY),
    host.clientHeight || window.innerHeight,
  );
  if (storedHeight !== null) {
    host.style.setProperty(
      "--embedded-shell-height",
      `${storedHeight}px`,
    );
  }

  function shellById(id: string | null) {
    return shells.find((shell) => shell.id === id);
  }

  function sendResize(shell: EmbeddedShellTab) {
    if (shell.state === "exited") return;
    void invoke("resize_session", {
      id: shell.id,
      rows: shell.term.rows,
      cols: shell.term.cols,
    }).catch(() => {});
  }

  function refit() {
    if (!opened) return;
    const shell = shellById(activeShellId);
    if (!shell) return;
    shell.fit.fit();
    sendResize(shell);
  }

  function runningCount(): number {
    return shells.filter((shell) => shell.state !== "exited").length;
  }

  function updateAppearance(theme: ITheme, fontSize: number) {
    for (const shell of shells) {
      shell.term.options.theme = theme;
      shell.term.options.fontSize = fontSize;
    }
    refit();
  }

  function renderTabs() {
    for (const shell of shells) {
      shell.tabEl.classList.toggle("active", shell.id === activeShellId);
      shell.tabEl.classList.toggle("exited", shell.state === "exited");
    }
  }

  function selectShell(id: string) {
    const selected = shellById(id);
    if (!selected) return;
    activeShellId = id;
    for (const shell of shells) shell.pane.classList.toggle("active", shell.id === id);
    pathEl.textContent = selected.project;
    pathEl.title = selected.project;
    renderTabs();
    requestAnimationFrame(() => {
      if (shellById(id) !== selected || activeShellId !== id) return;
      refit();
      selected.term.focus();
    });
  }

  function show() {
    if (opened) return;
    opened = true;
    host.classList.add("shell-dock-open");
    panel.setAttribute("aria-hidden", "false");
    options.afterLayoutChange();
  }

  function hide() {
    if (!opened) return;
    opened = false;
    host.classList.remove("shell-dock-open");
    panel.setAttribute("aria-hidden", "true");
    options.afterLayoutChange();
    options.focusMainTerminal();
  }

  function finalizeShellRemoval(id: string, selectNext: boolean) {
    const index = shells.findIndex((shell) => shell.id === id);
    if (index < 0) return;
    const [shell] = shells.splice(index, 1);
    shell.term.dispose();
    shell.pane.remove();
    shell.tabEl.remove();
    if (activeShellId === id) {
      activeShellId = null;
      if (!selectNext) return;
      const next = shells[index] || shells[index - 1];
      if (next) selectShell(next.id);
      else hide();
    }
  }

  async function closeShell(id: string, selectNext = true) {
    const shell = shellById(id);
    if (!shell || shell.closing) return;
    shell.closing = true;
    await shell.ready;
    if (shell.state === "running") {
      await invoke("close_session", { id: shell.id }).catch(() => {});
    }
    finalizeShellRemoval(id, selectNext);
  }

  async function requestCloseShell(id: string) {
    const shell = shellById(id);
    if (!shell) return;
    const confirmed = await options.confirm(t("embedded_shell_close_confirm", shell.title));
    if (confirmed) await closeShell(id);
  }

  async function requestCloseAllShells() {
    if (shells.length === 0) return;
    const ids = shells.map((shell) => shell.id);
    const confirmed = await options.confirm(t("embedded_shell_close_all_confirm", ids.length));
    if (!confirmed) return;
    await Promise.all(ids.map((id) => closeShell(id, false)));
    activeShellId = null;
    const remaining = shells[0];
    if (remaining) selectShell(remaining.id);
    else hide();
  }

  async function createForProject(project: string | null) {
    const normalized = project?.trim();
    if (!normalized) return;
    if (shells.length >= MAX_EMBEDDED_SHELLS) {
      shellById(activeShellId)?.term.writeln(`\r\n\x1b[33m${t("embedded_shell_limit")}\x1b[0m`);
      return;
    }
    show();
    const id = `embedded-shell-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    const term = new Terminal(options.terminalOptions());
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    const pane = document.createElement("div");
    pane.className = "embedded-shell-pane";
    terminalsEl.appendChild(pane);
    term.open(pane);

    const basename = normalized.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || normalized;
    const number = shells.filter((shell) => shell.project === normalized).length + 1;
    const title = number === 1 ? basename : `${basename} ${number}`;
    const tabEl = document.createElement("button");
    tabEl.type = "button";
    tabEl.className = "embedded-shell-tab";
    const label = document.createElement("span");
    label.textContent = title;
    const close = document.createElement("span");
    close.className = "embedded-shell-tab-close";
    close.textContent = "×";
    tabEl.append(label, close);
    tabsEl.appendChild(tabEl);

    const shell: EmbeddedShellTab = {
      id,
      project: normalized,
      title,
      state: "launching",
      ready: Promise.resolve(),
      closing: false,
      term,
      fit,
      pane,
      tabEl,
    };
    shells.push(shell);
    tabEl.onclick = () => selectShell(id);
    close.onclick = (event) => {
      event.stopPropagation();
      void requestCloseShell(id);
    };
    term.onData((data) => {
      if (shell.state === "running") {
        void invoke("write_session", { id, data }).catch(() => {});
      }
    });
    selectShell(id);

    shell.ready = (async () => {
      try {
        fit.fit();
        await invoke<SessionInfo>("create_session", {
          opts: {
            id,
            tool: "shell",
            project: normalized,
            relay: null,
            rows: term.rows,
            cols: term.cols,
          },
        });
        if (shell.state !== "exited") shell.state = "running";
        if (shell.closing || shell.state === "exited") return;
        sendResize(shell);
        if (activeShellId === id) term.focus();
      } catch (error) {
        shell.state = "exited";
        if (!shell.closing && shellById(id) === shell) {
          term.writeln(`\r\n\x1b[31m${String(error)}\x1b[0m`);
          renderTabs();
        }
      }
    })();
    await shell.ready;
  }

  async function toggleForProject(project: string | null) {
    const action = embeddedShellAction(opened, activeShellId, project, shells);
    if (action.kind === "disabled") return;
    if (action.kind === "hide") {
      hide();
      return;
    }
    if (action.kind === "select") {
      show();
      selectShell(action.id);
      return;
    }
    await createForProject(action.project);
  }

  addBtn.onclick = () => void createForProject(options.currentProject());
  hideBtn.onclick = hide;
  closeAllBtn.onclick = () => void requestCloseAllShells();

  resizer.addEventListener("pointerdown", (event) => {
    event.preventDefault();
    resizing = true;
    resizer.setPointerCapture(event.pointerId);
  });
  resizer.addEventListener("pointermove", (event) => {
    if (!resizing) return;
    const bounds = host.getBoundingClientRect();
    const height = embeddedShellHeight(bounds.bottom - event.clientY, bounds.height);
    host.style.setProperty("--embedded-shell-height", `${height}px`);
    options.afterLayoutChange();
    refit();
  });
  const finishResize = (event: PointerEvent) => {
    if (!resizing) return;
    resizing = false;
    if (resizer.hasPointerCapture(event.pointerId)) resizer.releasePointerCapture(event.pointerId);
    const height = panel.getBoundingClientRect().height;
    localStorage.setItem(HEIGHT_STORAGE_KEY, String(height));
    options.afterLayoutChange();
    refit();
  };
  resizer.addEventListener("pointerup", finishResize);
  resizer.addEventListener("pointercancel", finishResize);

  new ResizeObserver(refit).observe(terminalsEl);
  void listen<OutputEvent>("session://output", (event) => {
    shellById(event.payload.id)?.term.write(new Uint8Array(event.payload.data));
  }).catch(() => {});
  void listen<StatusEvent>("session://status", (event) => {
    const shell = shellById(event.payload.id);
    if (!shell || event.payload.state !== "exited") return;
    shell.state = "exited";
    if (!shell.closing) {
      shell.term.writeln(
        `\r\n\x1b[90m${t("embedded_shell_exited", event.payload.code ?? 0)}\x1b[0m`,
      );
    }
    renderTabs();
  }).catch(() => {});

  return {
    toggleForProject,
    createForProject,
    hide,
    refit,
    runningCount,
    updateAppearance,
  };
}
