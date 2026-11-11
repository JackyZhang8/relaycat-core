import { createHighlighterCore } from "shiki/core";
import { createOnigurumaEngine } from "shiki/engine/oniguruma";
import githubDark from "shiki/themes/github-dark.mjs";
import githubLight from "shiki/themes/github-light.mjs";
import { parseDiffPreviewLine, previewLanguage } from "./workspace-model";
import {
  canHighlight,
  normalizeHighlightLanguage,
  type HighlightRequest,
  type HighlightResponse,
  type SyntaxLines,
} from "./syntax-highlight-model";

// Explicit imports keep the shipped grammar set bounded and independently loadable.
const languages = {
  typescript: () => import("shiki/langs/typescript.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  jsx: () => import("shiki/langs/jsx.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  shellscript: () => import("shiki/langs/shellscript.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  jsonc: () => import("shiki/langs/jsonc.mjs"),
  json5: () => import("shiki/langs/json5.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  scss: () => import("shiki/langs/scss.mjs"),
  less: () => import("shiki/langs/less.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  swift: () => import("shiki/langs/swift.mjs"),
  c: () => import("shiki/langs/c.mjs"),
  cpp: () => import("shiki/langs/cpp.mjs"),
  csharp: () => import("shiki/langs/csharp.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  php: () => import("shiki/langs/php.mjs"),
  ruby: () => import("shiki/langs/ruby.mjs"),
  "objective-c": () => import("shiki/langs/objective-c.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  vue: () => import("shiki/langs/vue.mjs"),
  svelte: () => import("shiki/langs/svelte.mjs"),
  dockerfile: () => import("shiki/langs/dockerfile.mjs"),
  makefile: () => import("shiki/langs/makefile.mjs"),
  cmake: () => import("shiki/langs/cmake.mjs"),
  groovy: () => import("shiki/langs/groovy.mjs"),
  graphql: () => import("shiki/langs/graphql.mjs"),
  protobuf: () => import("shiki/langs/proto.mjs"),
  dart: () => import("shiki/langs/dart.mjs"),
  lua: () => import("shiki/langs/lua.mjs"),
  scala: () => import("shiki/langs/scala.mjs"),
  hcl: () => import("shiki/langs/hcl.mjs"),
  nix: () => import("shiki/langs/nix.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  properties: () => import("shiki/langs/properties.mjs"),
  dotenv: () => import("shiki/langs/dotenv.mjs"),
};

const highlighter = createHighlighterCore({
  themes: [githubDark, githubLight],
  langs: [],
  engine: createOnigurumaEngine(import("shiki/wasm")),
});

async function highlight(code: string, language: string | null): Promise<SyntaxLines | null> {
  if (!language || !canHighlight(code)) return null;
  const name = normalizeHighlightLanguage(language);
  if (!Object.hasOwn(languages, name)) return null;
  const engine = await highlighter;
  if (!engine.getLoadedLanguages().includes(name)) {
    const grammar = await languages[name as keyof typeof languages]();
    await engine.loadLanguage(grammar.default);
  }
  const tokens = engine.codeToTokensWithThemes(code, {
    lang: name,
    themes: { dark: "github-dark", light: "github-light" },
  });
  let tokenCount = 0;
  const result = code.split("\n").map((source, index) => {
    const line = (tokens[index] ?? []).map((token) => ({
      text: token.content,
      dark: token.variants.dark.color ?? "#e1e4e8",
      light: token.variants.light.color ?? "#24292e",
      darkFontStyle: Math.max(0, token.variants.dark.fontStyle ?? 0),
      lightFontStyle: Math.max(0, token.variants.light.fontStyle ?? 0),
    }));
    // Shiki strips CR from CRLF line endings; retain it in the preview text.
    if (source.endsWith("\r") && line.map((token) => token.text).join("") === source.slice(0, -1)) {
      line.push({ text: "\r", dark: "inherit", light: "inherit", darkFontStyle: 0, lightFontStyle: 0 });
    }
    tokenCount += line.length;
    return line;
  });
  if (tokenCount > 50_000 || result.map((line) => line.map((token) => token.text).join("")).join("\n") !== code) return null;
  return result;
}

async function highlightDiff(code: string, sourcePath: string): Promise<SyntaxLines> {
  const result: SyntaxLines = code.split("\n").map(() => []);
  let oldPath = sourcePath;
  let newPath = sourcePath;
  let inHunk = false;
  let before: Array<{ index: number; source: string }> = [];
  let after: Array<{ index: number; source: string }> = [];
  const flush = async () => {
    for (const [rows, path] of [[before, oldPath], [after, newPath]] as const) {
      if (!rows.length) continue;
      const tokens = await highlight(rows.map((row) => row.source).join("\n"), previewLanguage(path));
      if (tokens) rows.forEach((row, index) => { result[row.index] = tokens[index]; });
    }
    before = [];
    after = [];
  };
  const lines = code.split("\n");
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (line.startsWith("diff ")) {
      await flush();
      inHunk = false;
      oldPath = newPath = sourcePath;
    }
    if (!inHunk) {
      if (line.startsWith("--- a/")) oldPath = line.slice(6).split("\t")[0];
      if (line.startsWith("+++ b/")) newPath = line.slice(6).split("\t")[0];
    }
    const parsed = parseDiffPreviewLine(line, inHunk);
    if (parsed.kind === "hunk") {
      await flush();
      inHunk = true;
    } else if (inHunk && parsed.prefix) {
      const row = { index, source: parsed.source };
      if (parsed.kind !== "add") before.push(row);
      if (parsed.kind !== "del") after.push(row);
    }
  }
  await flush();
  return result;
}

// Serialize jobs so language loading and tokenization share one highlighter.
let queue = Promise.resolve();
self.onmessage = (event: MessageEvent<HighlightRequest>) => {
  const request = event.data;
  queue = queue.then(async () => {
    let lines: SyntaxLines | null = null;
    try {
      if (canHighlight(request.code)) lines = request.diffPath !== undefined
        ? await highlightDiff(request.code, request.diffPath)
        : await highlight(request.code, request.language);
    } catch { /* Plain text remains visible when a grammar cannot be loaded. */ }
    const response: HighlightResponse = { id: request.id, lines };
    self.postMessage(response);
  });
};
