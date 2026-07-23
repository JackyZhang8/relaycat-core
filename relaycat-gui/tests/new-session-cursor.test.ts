import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");

test("new-session clickable tool and project areas use the hand cursor", () => {
  assert.match(
    css,
    /#ov-new #tool-chips \.chip:not\(\.disabled\),\s*#ov-new #proj-list \.row\s*{\s*cursor:\s*pointer;/,
  );
});
