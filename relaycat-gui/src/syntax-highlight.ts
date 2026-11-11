import {
  canHighlight,
  HIGHLIGHT_MAX_CHARS,
  normalizeHighlightLanguage,
  type HighlightRequest,
  type HighlightResponse,
  type SyntaxLines,
} from "./syntax-highlight-model";

let worker: Worker | null = null;
let unavailable = false;
let nextId = 0;
const pending = new Map<number, {
  resolve: (lines: SyntaxLines | null) => void;
  timer: number;
}>();

function stopWorker() {
  worker?.terminate();
  worker = null;
  unavailable = true;
  for (const request of pending.values()) {
    window.clearTimeout(request.timer);
    request.resolve(null);
  }
  pending.clear();
}

function getWorker(): Worker | null {
  if (unavailable || typeof Worker === "undefined") return null;
  if (worker) return worker;
  try {
    worker = new Worker(new URL("./syntax-highlight.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (event: MessageEvent<HighlightResponse>) => {
      const request = pending.get(event.data.id);
      if (!request) return;
      window.clearTimeout(request.timer);
      pending.delete(event.data.id);
      request.resolve(event.data.lines);
    };
    worker.onerror = (event) => { event.preventDefault(); stopWorker(); };
    worker.onmessageerror = stopWorker;
    return worker;
  } catch {
    stopWorker();
    return null;
  }
}

export function highlightSyntax(code: string, language: string | null, diffPath?: string): Promise<SyntaxLines | null> {
  if (!language || ["plain", "text", "txt"].includes(language) || !canHighlight(code) || pending.size >= 8) return Promise.resolve(null);
  const target = getWorker();
  if (!target) return Promise.resolve(null);
  const id = ++nextId;
  return new Promise((resolve) => {
    // A pathological grammar must not occupy a CPU indefinitely.
    const timer = window.setTimeout(stopWorker, 15_000);
    pending.set(id, { resolve, timer });
    const request: HighlightRequest = { id, code, language: normalizeHighlightLanguage(language), diffPath };
    try { target.postMessage(request); } catch { stopWorker(); }
  });
}

export function applySyntaxTokens(element: HTMLElement, tokens: SyntaxLines[number]) {
  const fragment = document.createDocumentFragment();
  for (const token of tokens) {
    const span = document.createElement("span");
    span.className = "ws-syntax-token";
    span.textContent = token.text;
    span.style.setProperty("--syntax-dark", token.dark);
    span.style.setProperty("--syntax-light", token.light);
    if (token.darkFontStyle || token.lightFontStyle) {
      for (const [theme, flags] of [["dark", token.darkFontStyle], ["light", token.lightFontStyle]] as const) {
        span.style.setProperty(`--syntax-${theme}-style`, flags & 1 ? "italic" : "normal");
        span.style.setProperty(`--syntax-${theme}-weight`, flags & 2 ? "600" : "400");
        span.style.setProperty(`--syntax-${theme}-decoration`, flags & 4 ? "underline" : "none");
      }
    }
    fragment.appendChild(span);
  }
  element.replaceChildren(fragment);
}

export async function applySyntaxRows(rows: HTMLElement[], lines: SyntaxLines, isCurrent: () => boolean): Promise<void> {
  let deadline = performance.now() + 8;
  for (let index = 0; index < rows.length; index++) {
    if (!isCurrent()) return;
    if (lines[index]?.length) applySyntaxTokens(rows[index], lines[index]);
    if (performance.now() >= deadline) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      deadline = performance.now() + 8;
    }
  }
}

export async function highlightMarkdownBlocks(container: HTMLElement, isCurrent: () => boolean): Promise<void> {
  let remaining = HIGHLIGHT_MAX_CHARS;
  const blocks = container.querySelectorAll<HTMLElement>("pre > code");
  for (const block of blocks) {
    if (!isCurrent()) return;
    const language = block.className.match(/\blanguage-([\w+#.-]+)/)?.[1];
    const code = block.textContent ?? "";
    if (!language || code.length > Math.min(remaining, 50_000) || !canHighlight(code)) continue;
    remaining -= code.length;
    const lines = await highlightSyntax(code, language);
    if (!isCurrent() || !container.contains(block)) return;
    if (!lines) continue;
    const tokens = lines.flatMap((line, index) => index === 0 ? line : [
      { text: "\n", dark: "inherit", light: "inherit", darkFontStyle: 0, lightFontStyle: 0 }, ...line,
    ]);
    // Preserve the exact block text, including its final newline.
    if (tokens.map((token) => token.text).join("") === code) applySyntaxTokens(block, tokens);
  }
}
