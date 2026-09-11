import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { appendedTextareaText, truncateUtf8 } from "../src/terminal-input.ts";

const mainSource = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");

test("IME fallback never turns a cleared helper textarea into terminal backspaces", () => {
  assert.equal(appendedTextareaText("未发送的输入", ""), "");
});

test("IME fallback ignores replacement text and only forwards an appended suffix", () => {
  assert.equal(appendedTextareaText("旧草稿", "粘贴内容"), "");
  assert.equal(appendedTextareaText("已有", "已有新增"), "新增");
});

test("custom key handling consumes keydown 229 before xterm schedules a second textarea diff", () => {
  const handler = mainSource.match(
    /term\.attachCustomKeyEventHandler\(\(ev\) => \{([\s\S]*?)\n  \}\);\n  \/\/ On blur/,
  );
  assert.ok(handler, "custom key handler not found");
  assert.match(
    handler[1],
    /if \(ev\.type === "keydown" && ev\.keyCode === 229\) \{[\s\S]*?return false;\n    \}/,
  );
});


test("truncates pasted UTF-8 without splitting a character", () => {
  assert.equal(truncateUtf8("a😀b", 5), "a😀");
});
