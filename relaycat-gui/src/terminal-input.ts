// xterm's textarea is an ephemeral IME/clipboard helper, not an editor buffer.
// Blur, paste, Enter and Ctrl-C may clear or replace it wholesale. Only a
// strict append can represent text that the keydown(229) fallback should send.
export function appendedTextareaText(oldValue: string, newValue: string): string {
  return newValue.startsWith(oldValue) ? newValue.substring(oldValue.length) : "";
}
