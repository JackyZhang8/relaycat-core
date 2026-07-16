export type RelayCompatibilityCheck = {
  status: "compatible" | "incompatible" | "unverified";
  action?: "upgrade_gui" | "upgrade_server" | null;
  server_version?: string | null;
  min_gui_version?: string | null;
  reason: string;
};

type RelayCompatibilityProbe = (relayUrl: string) => Promise<RelayCompatibilityCheck>;

export function createRelayCompatibilityChecker(probe: RelayCompatibilityProbe) {
  const compatibleRelayCache = new Set<string>();

  return async (relayUrl: string): Promise<RelayCompatibilityCheck> => {
    const key = relayUrl.trim();
    if (compatibleRelayCache.has(key)) {
      return { status: "compatible", reason: "cached" };
    }
    const result = await probe(key);
    if (result.status === "compatible") compatibleRelayCache.add(key);
    return result;
  };
}
