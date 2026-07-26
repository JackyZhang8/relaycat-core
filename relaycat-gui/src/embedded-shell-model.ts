export interface EmbeddedShellSummary {
  id: string;
  project: string;
}

export interface OwnedEmbeddedShellSummary {
  ownerSessionId: string;
  number: number;
}

export const MAX_EMBEDDED_SHELLS_PER_SESSION = 5;

export function nextLocalShellNumber(
  ownerSessionId: string,
  shells: readonly OwnedEmbeddedShellSummary[],
): number | null {
  const used = new Set(
    shells
      .filter((shell) => shell.ownerSessionId === ownerSessionId)
      .map((shell) => shell.number),
  );
  for (let number = 2; number <= MAX_EMBEDDED_SHELLS_PER_SESSION; number += 1) {
    if (!used.has(number)) return number;
  }
  return null;
}

export function embeddedShellCreateKind(
  ownerSessionId: string,
  shells: readonly (OwnedEmbeddedShellSummary & { kind: "shared" | "local" })[],
): "shared" | "local" | "limit" {
  const owned = shells.filter((shell) => shell.ownerSessionId === ownerSessionId);
  if (!owned.some((shell) => shell.kind === "shared")) return "shared";
  return nextLocalShellNumber(ownerSessionId, owned) === null ? "limit" : "local";
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
