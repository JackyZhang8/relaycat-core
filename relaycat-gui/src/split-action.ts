export function runSplitAction(
  closeWorkspace: () => void,
  toggleLayout: () => void,
): void {
  closeWorkspace();
  toggleLayout();
}
