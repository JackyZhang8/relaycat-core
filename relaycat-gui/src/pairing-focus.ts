export function restoreTerminalFocusAfterOverlayClose(
  overlayId: string | undefined,
  focusActiveTerminal: () => void,
): boolean {
  if (overlayId !== "ov-pair") return false;
  focusActiveTerminal();
  return true;
}
