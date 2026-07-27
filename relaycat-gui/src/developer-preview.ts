export type DeveloperPreviewKind = "json" | "json_lines" | "table" | "svg" | "diff" | null;

export interface JsonPreviewNode {
  key?: string;
  kind: "object" | "array" | "value";
  value?: string;
  children?: JsonPreviewNode[];
  truncated: boolean;
}

export interface JsonPreviewRow {
  line: number;
  node?: JsonPreviewNode;
  error?: string;
}

export interface TablePreview {
  headers: string[];
  rows: string[][];
  truncated: boolean;
}

export function developerPreviewKind(path: string): DeveloperPreviewKind {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (/\.(?:csv|tsv)$/.test(name)) return "table";
  if (/\.svg$/.test(name)) return "svg";
  if (/\.(?:diff|patch)$/.test(name)) return "diff";
  return null;
}

export function parseJsonPreview(source: string, maxNodes = 2_000): JsonPreviewNode {
  const value = JSON.parse(stripJsonCommentsAndTrailingCommas(source));
  let count = 0;
  let wasTruncated = false;
  const build = (current: unknown, key?: string): JsonPreviewNode => {
    count += 1;
    if (count > Math.max(1, maxNodes)) {
      wasTruncated = true;
      return { key, kind: "value", value: "…", truncated: true };
    }
    if (Array.isArray(current)) {
      const children: JsonPreviewNode[] = [];
      for (let index = 0; index < current.length; index += 1) {
        if (count >= maxNodes) { wasTruncated = true; break; }
        children.push(build(current[index], String(index)));
      }
      return { key, kind: "array", children, truncated: wasTruncated };
    }
    if (current !== null && typeof current === "object") {
      const children: JsonPreviewNode[] = [];
      for (const [childKey, childValue] of Object.entries(current)) {
        if (count >= maxNodes) { wasTruncated = true; break; }
        children.push(build(childValue, childKey));
      }
      return { key, kind: "object", children, truncated: wasTruncated };
    }
    const valueText = typeof current === "string" ? current : JSON.stringify(current);
    return { key, kind: "value", value: valueText ?? "null", truncated: false };
  };
  const root = build(value);
  root.truncated = root.truncated || wasTruncated;
  return root;
}

export function parseJsonLinesPreview(source: string, maxRows = 200): JsonPreviewRow[] {
  return source.split(/\r?\n/).filter((line) => line.trim().length > 0).slice(0, Math.max(1, maxRows)).map((line, index) => {
    try {
      return { line: index + 1, node: parseJsonPreview(line) };
    } catch {
      return { line: index + 1, error: "Invalid JSON" };
    }
  });
}

export function parseDelimitedPreview(
  source: string,
  delimiter: "," | "\t",
  maxRows = 100,
  maxColumns = 30,
): TablePreview {
  const parsed: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  for (let index = 0; index <= source.length; index += 1) {
    const character = source[index] ?? "\n";
    if (quoted) {
      if (character === '"' && source[index + 1] === '"') { field += '"'; index += 1; }
      else if (character === '"') quoted = false;
      else field += character;
      continue;
    }
    if (character === '"' && field.length === 0) { quoted = true; continue; }
    if (character === delimiter) { row.push(field); field = ""; continue; }
    if (character === "\n" || character === "\r") {
      if (character === "\r" && source[index + 1] === "\n") index += 1;
      row.push(field); field = "";
      if (row.some((value) => value.length > 0)) parsed.push(row);
      row = [];
      continue;
    }
    field += character;
  }
  const columnLimit = Math.max(1, maxColumns);
  const rowLimit = Math.max(1, maxRows);
  const headers = (parsed[0] ?? []).slice(0, columnLimit);
  const rows = parsed.slice(1, rowLimit + 1).map((values) => values.slice(0, columnLimit));
  const truncated = parsed.length - 1 > rowLimit || parsed.some((values) => values.length > columnLimit);
  return { headers, rows, truncated };
}

export function sanitizeSvgPreview(source: string): string {
  if (!/<svg\b/i.test(source)) throw new Error("Invalid SVG");
  let safe = source
    .replace(/<\?(?:.|\n)*?\?>/g, "")
    .replace(/<!DOCTYPE(?:.|\n)*?>/gi, "")
    .replace(/<(script|foreignObject|iframe|object|embed|style)\b[\s\S]*?<\/\1\s*>/gi, "")
    .replace(/<(script|foreignObject|iframe|object|embed|style)\b[^>]*\/?\s*>/gi, "")
    .replace(/\s+on[a-z][\w:-]*\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/\s+(?:href|xlink:href|src)\s*=\s*(?:"(?!#)[^"]*"|'(?!#)[^']*'|(?!(?:#|\s|>))[^\s>]+)/gi, "")
    .replace(/url\(\s*(?!['"]?#)[^)]+\)/gi, "none");
  const start = safe.search(/<svg\b/i);
  const end = safe.toLowerCase().lastIndexOf("</svg>");
  if (start < 0 || end < start) throw new Error("Invalid SVG");
  safe = safe.slice(start, end + 6);
  return safe;
}

function stripJsonCommentsAndTrailingCommas(source: string): string {
  let output = "";
  let quote = "";
  let escaped = false;
  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (quote) {
      if (escaped) {
        output += quote === "'" && character === "'" ? "'" : `\\${character}`;
        escaped = false;
      } else if (character === "\\") escaped = true;
      else if (character === quote) { output += '"'; quote = ""; }
      else { output += quote === "'" && character === '"' ? '\\"' : character; }
      continue;
    }
    if (character === '"' || character === "'") { quote = character; output += '"'; continue; }
    if (character === "/" && source[index + 1] === "/") {
      while (index < source.length && source[index] !== "\n") index += 1;
      output += "\n";
      continue;
    }
    if (character === "/" && source[index + 1] === "*") {
      index += 2;
      while (index < source.length && !(source[index] === "*" && source[index + 1] === "/")) index += 1;
      index += 1;
      continue;
    }
    output += character;
  }
  return output
    .replace(/([,{]\s*)([A-Za-z_$][\w$]*)(\s*:)/g, '$1"$2"$3')
    .replace(/,\s*([}\]])/g, "$1");
}
