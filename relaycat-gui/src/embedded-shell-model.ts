export interface EmbeddedShellSummary {
  id: string;
  project: string;
}

export type EmbeddedShellAction =
  | { kind: "disabled" }
  | { kind: "select"; id: string }
  | { kind: "create"; project: string };

export function embeddedShellAction(
  requestedProject: string | null,
  shells: readonly EmbeddedShellSummary[],
): EmbeddedShellAction {
  const project = requestedProject?.trim();
  if (!project) return { kind: "disabled" };
  const existing = shells.find((shell) => shell.project === project);
  return existing
    ? { kind: "select", id: existing.id }
    : { kind: "create", project };
}

export type TermSidePanelMode = "files" | "git" | "history" | "shell" | null;

export function toggleTermSidePanel(
  current: TermSidePanelMode,
  requested: Exclude<TermSidePanelMode, null>,
): TermSidePanelMode {
  if (requested === "git" && (current === "git" || current === "history")) return null;
  return current === requested ? null : requested;
}
