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
markdown.renderer.rules.link_open = () => '<span class="ws-md-link">';
markdown.renderer.rules.link_close = () => "</span>";

function decorateTaskItems(html: string): string {
  return html.replace(
    /<li>\[([ xX])\]\s*/g,
    (_match, checked: string) =>
      `<li class="ws-md-task"><span class="ws-md-task-box" aria-hidden="true">${checked.toLowerCase() === "x" ? "✓" : ""}</span>`,
  );
}

export function renderSafeMarkdown(source: string): string {
  const rendered = decorateTaskItems(markdown.render(source));
  if (typeof window === "undefined") return rendered;
  const purifier = createDOMPurify(window);
  return purifier.sanitize(rendered, {
    FORBID_TAGS: ["a", "img", "script", "style", "iframe", "object", "embed"],
    FORBID_ATTR: ["href", "src", "srcset", "style"],
  });
}
