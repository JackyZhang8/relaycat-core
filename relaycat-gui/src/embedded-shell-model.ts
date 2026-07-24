export interface EmbeddedShellSummary {
  id: string;
  project: string;
}

export type EmbeddedShellAction =
  | { kind: "disabled" }
  | { kind: "hide" }
  | { kind: "select"; id: string }
  | { kind: "create"; project: string };

export function embeddedShellAction(
  opened: boolean,
  activeShellId: string | null,
  requestedProject: string | null,
  shells: readonly EmbeddedShellSummary[],
): EmbeddedShellAction {
  const project = requestedProject?.trim();
  if (!project) return { kind: "disabled" };
  const active = shells.find((shell) => shell.id === activeShellId);
  if (active?.project === project) {
    return opened ? { kind: "hide" } : { kind: "select", id: active.id };
  }
  const existing = shells.find((shell) => shell.project === project);
  return existing
    ? { kind: "select", id: existing.id }
    : { kind: "create", project };
}

export function embeddedShellHeight(requested: number, hostHeight: number): number {
  return Math.min(Math.max(requested, 160), Math.max(160, hostHeight * 0.6));
}

export function storedEmbeddedShellHeight(
  stored: string | null,
  hostHeight: number,
): number | null {
  if (stored === null) return null;
  const value = Number(stored);
  return Number.isFinite(value) ? embeddedShellHeight(value, hostHeight) : null;
}

export type TermSidePanelMode = "files" | "git" | "history" | null;

export function toggleTermSidePanel(
  current: TermSidePanelMode,
  requested: Exclude<TermSidePanelMode, null>,
): TermSidePanelMode {
  if (requested === "git" && (current === "git" || current === "history")) return null;
  return current === requested ? null : requested;
}
