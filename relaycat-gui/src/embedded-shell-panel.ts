import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ITheme } from "@xterm/xterm";

import {
  MAX_EMBEDDED_SHELLS_PER_SESSION,
  embeddedShellCreateKind,
  nextLocalShellNumber,
  shouldApplyWorkspaceOutput,
} from "./embedded-shell-model";
import { tabScrollState } from "./tab-scroll";

interface ActiveRelaySession {
  id: string;
  mode: string;
  project: string;
}

interface SessionInfo {
  id: string;
  title: string;
  mode: string;
  log_path: string | null;
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

interface WorkspaceTerminalSnapshot {
  available: boolean;
  shell_id?: string;
  data: number[];
  last_output_seq: number;
  exited: boolean;
  exit_code?: number;
}

interface WorkspaceTerminalEvent {
  id: string;
  kind: "ready" | "started" | "output" | "exit";
  shell_id?: string;
  data?: number[];
  output_seq?: number;
  code?: number;
}

interface EmbeddedShellTab {
  id: string;
  remoteShellId?: string;
  ownerSessionId: string;
  kind: "shared" | "local";
  number: number;
  project: string;
  title: string;
  state: "launching" | "running" | "exited";
  ready: Promise<void>;
  closing: boolean;
  snapshotApplied: boolean;
  lastAppliedOutputSeq: number;
  pendingOutput: Array<{ output_seq: number; data: number[] }>;
  term: Terminal;
  fit: FitAddon;
  pane: HTMLDivElement;
  tabEl: HTMLButtonElement;
  inputQueue: Promise<void>;
  inputGeneration: number;
}

interface EmbeddedShellPanelOptions {
  translate: (key: string, ...args: (string | number)[]) => string;
  terminalOptions: () => ConstructorParameters<typeof Terminal>[0];
  currentProject: () => string | null;
  currentSession: () => ActiveRelaySession | null;
  focusMainTerminal: () => void;
  afterLayoutChange: () => void;
  confirm: (message: string) => Promise<boolean>;
  notice: (message: string) => Promise<void>;
  onRequestClose: () => void;
}

export interface EmbeddedShellPanelController {
  activateForProject(project: string | null): Promise<void>;
  createForProject(project: string | null): Promise<void>;
  closeForSession(sessionId: string): Promise<void>;
  setVisible(visible: boolean): void;
  refit(): void;
  runningCount(): number;
  updateAppearance(theme: ITheme, fontSize: number): void;
}

export function createEmbeddedShellPanel(
  options: EmbeddedShellPanelOptions,
): EmbeddedShellPanelController {
  const { translate: t } = options;
  const panel = document.querySelector("#embedded-shell-panel") as HTMLElement;
  const tabsEl = document.querySelector("#embedded-shell-tabs") as HTMLElement;
  const terminalsEl = document.querySelector("#embedded-shell-terminals") as HTMLElement;
  const scrollLeftBtn = document.querySelector("#embedded-shell-scroll-left") as HTMLButtonElement;
  const scrollRightBtn = document.querySelector("#embedded-shell-scroll-right") as HTMLButtonElement;
  const addBtn = document.querySelector("#embedded-shell-add") as HTMLButtonElement;

  const shells: EmbeddedShellTab[] = [];
  const activeShellByOwner = new Map<string, string>();
  let activeOwnerSessionId: string | null = null;
  let localSequence = 0;
  let opened = false;

  function shellById(id: string | null) {
    return shells.find((shell) => shell.id === id);
  }

  function shellsForOwner(ownerSessionId: string) {
    return shells
      .filter((shell) => shell.ownerSessionId === ownerSessionId)
      .sort((left, right) => left.number - right.number);
  }

  function activeShell() {
    if (!activeOwnerSessionId) return undefined;
    return shellById(activeShellByOwner.get(activeOwnerSessionId) ?? null);
  }

  function sendResize(shell: EmbeddedShellTab) {
    if (shell.state === "exited") return;
    if (shell.kind === "shared") {
      void invoke("resize_workspace_terminal", {
        id: shell.id,
        rows: shell.term.rows,
        cols: shell.term.cols,
      }).catch(() => {});
    } else {
      void invoke("resize_session", {
        id: shell.id,
        rows: shell.term.rows,
        cols: shell.term.cols,
      }).catch(() => {});
    }
  }

  function refit() {
    if (!opened) return;
    const shell = activeShell();
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
    const activeId = activeOwnerSessionId
      ? activeShellByOwner.get(activeOwnerSessionId) ?? null
      : null;
    for (const shell of shells) {
      const visible = shell.ownerSessionId === activeOwnerSessionId;
      shell.tabEl.hidden = !visible;
      shell.tabEl.classList.toggle("active", visible && shell.id === activeId);
      shell.tabEl.classList.toggle("exited", shell.state === "exited");
      shell.pane.classList.toggle("active", visible && shell.id === activeId);
    }
    addBtn.disabled = !activeOwnerSessionId;
    requestAnimationFrame(updateShellTabScrollControls);
  }

  function updateShellTabScrollControls() {
    const state = tabScrollState(tabsEl);
    scrollLeftBtn.hidden = !state.overflow;
    scrollRightBtn.hidden = !state.overflow;
    scrollLeftBtn.disabled = !state.canScrollLeft;
    scrollRightBtn.disabled = !state.canScrollRight;
  }

  function scrollShellTabs(direction: -1 | 1) {
    tabsEl.scrollBy({
      left: direction * Math.max(120, Math.floor(tabsEl.clientWidth * 0.7)),
      behavior: "smooth",
    });
  }

  function selectShell(id: string) {
    const selected = shellById(id);
    if (!selected || selected.ownerSessionId !== activeOwnerSessionId) return;
    activeShellByOwner.set(selected.ownerSessionId, id);
    renderTabs();
    requestAnimationFrame(() => {
      if (!opened) return;
      if (
        shellById(id) !== selected ||
        activeShellByOwner.get(selected.ownerSessionId) !== id ||
        activeOwnerSessionId !== selected.ownerSessionId
      ) return;
      selected.tabEl.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
      refit();
      selected.term.focus();
    });
  }

  function setActiveOwner(sessionId: string) {
    activeOwnerSessionId = sessionId;
    const owned = shellsForOwner(sessionId);
    const remembered = shellById(activeShellByOwner.get(sessionId) ?? null);
    if (!remembered || remembered.ownerSessionId !== sessionId) {
      const fallback = owned[0];
      if (fallback) activeShellByOwner.set(sessionId, fallback.id);
      else activeShellByOwner.delete(sessionId);
    }
    renderTabs();
  }

  function setVisible(visible: boolean) {
    if (opened === visible) return;
    opened = visible;
    panel.setAttribute("aria-hidden", String(!visible));
    options.afterLayoutChange();
    if (visible) requestAnimationFrame(refit);
    else options.focusMainTerminal();
  }

  function finalizeShellRemoval(id: string, selectNext: boolean) {
    const index = shells.findIndex((shell) => shell.id === id);
    if (index < 0) return;
    const [shell] = shells.splice(index, 1);
    shell.term.dispose();
    shell.pane.remove();
    shell.tabEl.remove();

    if (activeShellByOwner.get(shell.ownerSessionId) === id) {
      activeShellByOwner.delete(shell.ownerSessionId);
      if (selectNext) {
        const next = shellsForOwner(shell.ownerSessionId)[0];
        if (next) activeShellByOwner.set(shell.ownerSessionId, next.id);
        else if (activeOwnerSessionId === shell.ownerSessionId) options.onRequestClose();
      }
    }
    renderTabs();
    const nextId = activeOwnerSessionId
      ? activeShellByOwner.get(activeOwnerSessionId) ?? null
      : null;
    if (selectNext && nextId) selectShell(nextId);
  }

  async function closeShell(id: string, selectNext = true, terminate = false) {
    const shell = shellById(id);
    if (!shell || shell.closing) return;
    shell.closing = true;
    shell.inputGeneration += 1;
    await shell.ready;
    if (shell.kind === "shared") {
      await invoke(terminate ? "close_workspace_terminal" : "detach_workspace_terminal", {
        id: shell.id,
      }).catch(() => {});
    } else {
      await invoke("close_session", { id: shell.id }).catch(() => {});
    }
    finalizeShellRemoval(id, selectNext);
  }

  async function requestCloseShell(id: string) {
    const shell = shellById(id);
    if (!shell) return;
    if (shell.kind === "shared") {
      const confirmed = await options.confirm(t("embedded_shell_close_confirm", shell.title));
      if (!confirmed) return;
      await closeShell(id, true, true);
      return;
    }
    const confirmed = shell.state === "exited" || await options.confirm(
      t("embedded_shell_close_local_confirm", shell.title),
    );
    if (confirmed) await closeShell(id, true, true);
  }

  function createShellTab(
    session: ActiveRelaySession,
    project: string,
    id: string,
    kind: "shared" | "local",
    number: number,
  ): EmbeddedShellTab {
    const term = new Terminal(options.terminalOptions());
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    const pane = document.createElement("div");
    pane.className = "embedded-shell-pane";
    terminalsEl.appendChild(pane);
    term.open(pane);

    const title = `Shell ${number}`;
    const tabEl = document.createElement("button");
    tabEl.type = "button";
    tabEl.className = "embedded-shell-tab";
    const label = document.createElement("span");
    label.textContent = title;
    const badge = document.createElement("span");
    badge.className = `embedded-shell-tab-kind ${kind}`;
    badge.textContent = t(
      kind === "shared" ? "embedded_shell_shared_badge" : "embedded_shell_local_badge",
    );
    const close = document.createElement("span");
    close.className = "embedded-shell-tab-close";
    close.textContent = "×";
    tabEl.append(label, badge, close);
    tabsEl.appendChild(tabEl);

    const shell: EmbeddedShellTab = {
      id,
      ownerSessionId: session.id,
      kind,
      number,
      project,
      title,
      state: "launching",
      ready: Promise.resolve(),
      closing: false,
      snapshotApplied: kind === "local",
      pendingOutput: [],
      term,
      fit,
      pane,
      tabEl,
      inputQueue: Promise.resolve(),
      inputGeneration: 0,
    };
    shells.push(shell);
    tabEl.onclick = () => selectShell(id);
    close.onclick = (event) => {
      event.stopPropagation();
      void requestCloseShell(id);
    };
    return shell;
  }

  async function createForProject(project: string | null, remoteShellId?: string) {
    const session = options.currentSession();
    const normalized = session?.project.trim() || project?.trim();
    if (!session || session.mode !== "relay" || !normalized) return;
    setActiveOwner(session.id);
    const existing = shells.find(
      (shell) => shell.ownerSessionId === session.id && shell.kind === "shared",
    );
    if (existing) {
      selectShell(activeShellByOwner.get(session.id) ?? existing.id);
      return;
    }
    if (shellsForOwner(session.id).length >= MAX_EMBEDDED_SHELLS_PER_SESSION) return;

    const id = session.id;
    const shell = createShellTab(session, normalized, id, "shared", 1);
    shell.remoteShellId = remoteShellId;
    shell.term.onData((data) => {
      if (shell.state === "running") {
        const generation = shell.inputGeneration;
        const bytes = Array.from(new TextEncoder().encode(data));
        shell.inputQueue = shell.inputQueue.then(async () => {
          if (generation !== shell.inputGeneration || shell.state !== "running") return;
          await invoke("write_workspace_terminal", { id: shell.id, data: bytes });
        }).catch(() => {});
      }
    });
    activeShellByOwner.set(session.id, id);
    selectShell(id);

    shell.ready = (async () => {
      try {
        shell.fit.fit();
        let snapshot: WorkspaceTerminalSnapshot | null = null;
        for (let attempt = 0; attempt < 20 && !snapshot; attempt += 1) {
          snapshot = await invoke<WorkspaceTerminalSnapshot>("attach_workspace_terminal", { id })
            .catch(() => null);
          if (!snapshot) await new Promise((resolve) => window.setTimeout(resolve, 100));
        }
        if (!snapshot) throw new Error(t("embedded_shell_bridge_unavailable"));
        if (shell.closing || shellById(id) !== shell) return;
        shell.remoteShellId = snapshot.shell_id;
        shell.lastAppliedOutputSeq = snapshot.last_output_seq;
        if (snapshot.data.length > 0) shell.term.write(new Uint8Array(snapshot.data));
        shell.snapshotApplied = true;
        for (const pending of shell.pendingOutput) {
          if (pending.output_seq > snapshot.last_output_seq && shouldApplyWorkspaceOutput(shell.lastAppliedOutputSeq, pending.output_seq)) {
            shell.term.write(new Uint8Array(pending.data));
            shell.lastAppliedOutputSeq = pending.output_seq;
          }
        }
        shell.pendingOutput = [];
        if (snapshot.exited) {
          shell.state = "exited";
          renderTabs();
          return;
        }
        shell.state = "running";
        sendResize(shell);
        if (activeShell() === shell) shell.term.focus();
      } catch (error) {
        shell.state = "exited";
        if (!shell.closing && shellById(id) === shell) {
          shell.term.writeln(`\r\n\x1b[31m${String(error)}\x1b[0m`);
          renderTabs();
        }
      }
    })();
    await shell.ready;
  }

  async function createLocalForProject(project: string | null) {
    const session = options.currentSession();
    const normalized = session?.project.trim() || project?.trim();
    if (!session || session.mode !== "relay" || !normalized) return;
    setActiveOwner(session.id);
    const number = nextLocalShellNumber(session.id, shells);
    if (number === null) {
      await options.notice(t("embedded_shell_limit"));
      return;
    }

    const id = `workspace-local:${session.id}:${number}:${++localSequence}`;
    const shell = createShellTab(session, normalized, id, "local", number);
    shell.term.onData((data) => {
      if (shell.state === "running") {
        const generation = shell.inputGeneration;
        shell.inputQueue = shell.inputQueue.then(async () => {
          if (generation !== shell.inputGeneration || shell.state !== "running") return;
          await invoke("write_session", { id: shell.id, data });
        }).catch(() => {});
      }
    });
    activeShellByOwner.set(session.id, id);
    selectShell(id);

    shell.ready = (async () => {
      try {
        shell.fit.fit();
        await invoke<SessionInfo>("create_session", {
          opts: {
            id: shell.id,
            tool: "shell",
            project: normalized,
            relay: null,
            rows: shell.term.rows,
            cols: shell.term.cols,
          },
        });
        if (shell.closing || shellById(id) !== shell) return;
        shell.state = "running";
        sendResize(shell);
        if (activeShell() === shell) shell.term.focus();
      } catch (error) {
        shell.state = "exited";
        if (!shell.closing && shellById(id) === shell) {
          shell.term.writeln(`\r\n\x1b[31m${String(error)}\x1b[0m`);
          renderTabs();
        }
      }
    })();
    await shell.ready;
  }

  async function createNextForProject(project: string | null) {
    const session = options.currentSession();
    if (!session || session.mode !== "relay") return;
    switch (embeddedShellCreateKind(session.id, shells)) {
      case "shared":
        await createForProject(project);
        return;
      case "local":
        await createLocalForProject(project);
        return;
      case "limit":
        await options.notice(t("embedded_shell_limit"));
    }
  }

  async function activateForProject(project: string | null) {
    const session = options.currentSession();
    if (!session || session.mode !== "relay") return;
    setActiveOwner(session.id);
    setVisible(true);
    const shared = shells.find(
      (shell) => shell.ownerSessionId === session.id && shell.kind === "shared",
    );
    if (shared) {
      selectShell(activeShellByOwner.get(session.id) ?? shared.id);
      return;
    }
    await createForProject(project);
  }

  async function closeForSession(sessionId: string): Promise<void> {
    const owned = shellsForOwner(sessionId);
    await Promise.all(owned.map((shell) => closeShell(shell.id, false, false)));
    activeShellByOwner.delete(sessionId);
    if (activeOwnerSessionId === sessionId) activeOwnerSessionId = null;
    renderTabs();
  }

  addBtn.onclick = () => void createNextForProject(options.currentProject());
  scrollLeftBtn.onclick = () => scrollShellTabs(-1);
  scrollRightBtn.onclick = () => scrollShellTabs(1);
  tabsEl.addEventListener("scroll", updateShellTabScrollControls, { passive: true });

  new ResizeObserver(refit).observe(terminalsEl);
  new ResizeObserver(updateShellTabScrollControls).observe(tabsEl);

  void listen<WorkspaceTerminalEvent>("session://workspace-terminal", (event) => {
    const shell = shells.find(
      (candidate) => candidate.kind === "shared" && candidate.id === event.payload.id,
    );
    if (!shell) {
      if (event.payload.kind !== "started") return;
      const session = options.currentSession();
      if (!session || session.mode !== "relay" || session.id !== event.payload.id) return;
      void createForProject(session.project, event.payload.shell_id);
      return;
    }
    if (
      event.payload.kind !== "started" &&
      event.payload.shell_id &&
      shell.remoteShellId &&
      event.payload.shell_id !== shell.remoteShellId
    ) return;
    if (event.payload.kind === "output" && event.payload.data) {
      const output_seq = event.payload.output_seq ?? 0;
      if (!shell.snapshotApplied) {
        shell.pendingOutput.push({ output_seq, data: event.payload.data });
        return;
      }
      if (!shouldApplyWorkspaceOutput(shell.lastAppliedOutputSeq, output_seq)) return;
      shell.term.write(new Uint8Array(event.payload.data));
      shell.lastAppliedOutputSeq = output_seq;
      return;
    }
    if (event.payload.kind === "started") {
      shell.remoteShellId = event.payload.shell_id;
      shell.state = "running";
      renderTabs();
      return;
    }
    if (event.payload.kind === "exit") {
      if (shell.closing) return;
      finalizeShellRemoval(shell.id, true);
      void options.notice(t("embedded_shell_closed_in_app"));
    }
  }).catch(() => {});

  void listen<OutputEvent>("session://output", (event) => {
    const shell = shells.find(
      (candidate) => candidate.kind === "local" && candidate.id === event.payload.id,
    );
    if (shell) shell.term.write(new Uint8Array(event.payload.data));
  }).catch(() => {});

  void listen<StatusEvent>("session://status", (event) => {
    const shell = shells.find(
      (candidate) => candidate.kind === "local" && candidate.id === event.payload.id,
    );
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
    activateForProject,
    createForProject,
    closeForSession,
    setVisible,
    refit,
    runningCount,
    updateAppearance,
  };
}
