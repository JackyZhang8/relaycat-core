// xterm's textarea is an ephemeral IME/clipboard helper, not an editor buffer.
// Blur, paste, Enter and Ctrl-C may clear or replace it wholesale. Only a
// strict append can represent text that the keydown(229) fallback should send.
export function appendedTextareaText(oldValue: string, newValue: string): string {
  return newValue.startsWith(oldValue) ? newValue.substring(oldValue.length) : "";
}

export function truncateUtf8(text: string, maxBytes: number): string {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length <= maxBytes) return text;
  let end = Math.max(0, maxBytes);
  while (end > 0) {
    try { return new TextDecoder("utf-8", { fatal: true }).decode(bytes.slice(0, end)); }
    catch { end -= 1; }
  }
  return "";
}
