export interface SyntaxToken {
  text: string;
  dark: string;
  light: string;
  darkFontStyle: number;
  lightFontStyle: number;
}

export type SyntaxLines = SyntaxToken[][];

export interface HighlightRequest {
  id: number;
  code: string;
  language: string;
  diffPath?: string;
}

export interface HighlightResponse {
  id: number;
  lines: SyntaxLines | null;
}

// Preview read limits are larger; these limits bound optional highlighting.
export const HIGHLIGHT_MAX_CHARS = 200_000;
export const HIGHLIGHT_MAX_LINES = 5_000;
export const HIGHLIGHT_MAX_LINE_LENGTH = 4_000;

export function canHighlight(code: string): boolean {
  if (!code || code.length > HIGHLIGHT_MAX_CHARS) return false;
  const lines = code.split("\n");
  return lines.length <= HIGHLIGHT_MAX_LINES &&
    lines.every((line) => line.length <= HIGHLIGHT_MAX_LINE_LENGTH);
}

export function normalizeHighlightLanguage(language: string): string {
  const name = language.trim().toLowerCase();
  const aliases: Record<string, string> = {
    ts: "typescript", js: "javascript", rb: "ruby", py: "python", rs: "rust",
    sh: "shellscript", shell: "shellscript", bash: "shellscript", zsh: "shellscript",
    yml: "yaml", md: "markdown", cs: "csharp", "c#": "csharp", "c++": "cpp",
    objc: "objective-c", objectivec: "objective-c", kt: "kotlin", html5: "html",
    gql: "graphql", proto: "protobuf", tf: "hcl", make: "makefile",
  };
  return aliases[name] ?? name;
}
