import createDOMPurify from "dompurify";
import MarkdownIt from "markdown-it";

const markdown = new MarkdownIt({
  breaks: false,
  html: false,
  linkify: false,
  typographer: false,
});

markdown.renderer.rules.image = (tokens, index) => {
  const alt = markdown.utils.escapeHtml(tokens[index].content || "image");
  return `<span class="ws-md-image-placeholder">[Image: ${alt}]</span>`;
};
/** Resolve a document link inside the project, without navigating the webview. */
export function resolveMarkdownFileLink(sourcePath: string, href: string): string | null {
  let path: string;
  try {
    path = decodeURIComponent(href.trim().split(/[?#]/, 1)[0]);
  } catch {
    return null;
  }
  if (
    !/\.(md|markdown)$/i.test(path) ||
    path.startsWith("//") ||
    /[:\\\u0000-\u001f\u007f]/.test(path)
  ) return null;
  const parts = path.startsWith("/") ? [] : sourcePath.split("/").slice(0, -1);
  for (const part of path.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (parts.length === 0) return null;
      parts.pop();
    } else {
      parts.push(part);
    }
  }
  return parts.length > 0 ? parts.join("/") : null;
}

markdown.renderer.rules.link_open = (tokens, index, _options, env) => {
  const path = resolveMarkdownFileLink(env?.sourcePath ?? "", tokens[index].attrGet("href") ?? "");
  if (path === null) return '<span class="ws-md-link">';
  return `<span class="ws-md-link ws-md-local-link" role="link" tabindex="0" data-workspace-path="${markdown.utils.escapeHtml(path)}">`;
};
markdown.renderer.rules.link_close = () => "</span>";

function decorateTaskItems(html: string): string {
  return html.replace(
    /<li>\[([ xX])\]\s*/g,
    (_match, checked: string) =>
      `<li class="ws-md-task"><span class="ws-md-task-box" aria-hidden="true">${checked.toLowerCase() === "x" ? "✓" : ""}</span>`,
  );
}

export function renderSafeMarkdown(source: string, sourcePath = ""): string {
  const rendered = decorateTaskItems(markdown.render(source, { sourcePath }));
  if (typeof window === "undefined") return rendered;
  const purifier = createDOMPurify(window);
  return purifier.sanitize(rendered, {
    FORBID_TAGS: ["a", "img", "script", "style", "iframe", "object", "embed"],
    FORBID_ATTR: ["href", "src", "srcset", "style"],
  });
}

let svgPurifier: ReturnType<typeof createDOMPurify> | null = null;

/**
 * Second sanitization pass for untrusted SVG markup before it is assigned to
 * `innerHTML`. The regex pre-sanitizer in developer-preview strips the obvious
 * active content, but regex sanitizers are fragile against mutation-based
 * bypasses, so the markup also goes through DOMPurify's SVG profile.
 */
export function purifySvgMarkup(markup: string): string {
  if (typeof window === "undefined") return markup;
  svgPurifier ??= createDOMPurify(window);
  return svgPurifier.sanitize(markup, {
    USE_PROFILES: { svg: true, svgFilters: true },
  });
}
