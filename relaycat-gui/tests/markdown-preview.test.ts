import assert from "node:assert/strict";
import test from "node:test";

import { renderSafeMarkdown } from "../src/markdown-preview.ts";

test("renders common Markdown blocks including tables tasks and code", () => {
  const html = renderSafeMarkdown(`# Title

- [x] done

| Name | Value |
| --- | --- |
| one | two |

\`\`\`ts
const value = 1;
\`\`\``);

  assert.match(html, /<h1>Title<\/h1>/);
  assert.match(html, /ws-md-task/);
  assert.match(html, /<table>/);
  assert.match(html, /<pre><code class="language-ts">/);
});

test("does not execute HTML load images or create navigable links", () => {
  const html = renderSafeMarkdown(`<script>alert(1)</script>

![diagram](https://example.test/image.png)

[unsafe](javascript:alert(1))

[safe](https://example.test)`);

  assert.doesNotMatch(html, /<script/i);
  assert.doesNotMatch(html, /<img/i);
  assert.doesNotMatch(html, /href=/i);
  assert.match(html, /ws-md-image-placeholder/);
  assert.match(html, /unsafe/);
  assert.match(html, /safe/);
});
