export type AndroidDownload = {
  version: string;
  url: string;
};

export function resolveAndroidDownload(manifest: unknown): AndroidDownload | null {
  if (!manifest || typeof manifest !== "object") return null;

  const candidate = manifest as {
    latest_version_name?: unknown;
    apk?: { url?: unknown };
  };
  const version =
    typeof candidate.latest_version_name === "string"
      ? candidate.latest_version_name.trim()
      : "";
  const url = typeof candidate.apk?.url === "string" ? candidate.apk.url.trim() : "";
  if (!version || !url) return null;

  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:") return null;
  } catch {
    return null;
  }

  return { version, url };
}

export function androidQrHref(url: string): string {
  return `https://api.qrserver.com/v1/create-qr-code/?size=216x216&data=${encodeURIComponent(url)}`;
}
