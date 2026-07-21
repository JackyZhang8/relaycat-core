export function shouldApplyRelaySnapshot(
  currentRevision: number,
  incomingRevision: number,
  exited: boolean,
): boolean {
  return !exited && incomingRevision > currentRevision;
}
