import assert from "node:assert/strict";
import test from "node:test";

import {
  developerPreviewKind,
  parseDelimitedPreview,
  parseJsonLinesPreview,
  parseJsonPreview,
  sanitizeSvgPreview,
} from "../src/developer-preview.ts";

test("developer preview leaves JSON-family files in source mode", () => {
  for (const path of ["config.json", "config.jsonc", "config.json5", "events.jsonl", "events.ndjson", "bundle.map"]) {
    assert.equal(developerPreviewKind(path), null);
  }
  assert.equal(developerPreviewKind("report.csv"), "table");
  assert.equal(developerPreviewKind("report.tsv"), "table");
  assert.equal(developerPreviewKind("logo.svg"), "svg");
  assert.equal(developerPreviewKind("change.patch"), "diff");
  assert.equal(developerPreviewKind("certificate.pem"), null);
});

test("developer preview builds a bounded JSON tree", () => {
  const preview = parseJsonPreview('{"name":"relaycat","items":[1,true,null]}');
  assert.equal(preview.kind, "object");
  assert.equal(preview.children?.[0].key, "name");
  assert.equal(preview.children?.[1].kind, "array");
  assert.equal(preview.truncated, false);

  const bounded = parseJsonPreview('[1,2,3,4]', 3);
  assert.equal(bounded.truncated, true);
  assert.equal(parseJsonPreview('{// note\n"enabled":true,}').children?.[0].key, "enabled");
  const json5 = parseJsonPreview("{'name':'relaycat', enabled:true}");
  assert.equal(json5.children?.find((child) => child.key === "name")?.value, "relaycat");
  assert.equal(json5.children?.find((child) => child.key === "enabled")?.value, "true");
});

test("developer preview parses JSON lines independently", () => {
  const rows = parseJsonLinesPreview('{"id":1}\nnot-json\n{"id":2}', 10);
  assert.equal(rows.length, 3);
  assert.equal(rows[0].node?.kind, "object");
  assert.equal(rows[1].error, "Invalid JSON");
  assert.equal(rows[2].line, 3);
});

test("developer preview parses quoted CSV and bounded TSV", () => {
  const csv = parseDelimitedPreview('name,note\nRelayCat,"hello, world"', ",");
  assert.deepEqual(csv.headers, ["name", "note"]);
  assert.deepEqual(csv.rows, [["RelayCat", "hello, world"]]);
  const tsv = parseDelimitedPreview("a\tb\n1\t2\n3\t4", "\t", 1, 2);
  assert.deepEqual(tsv.rows, [["1", "2"]]);
  assert.equal(tsv.truncated, true);
});

test("developer preview strips active and external SVG content", () => {
  const safe = sanitizeSvgPreview('<svg onload="alert(1)"><script>alert(1)</script><image href="https://example.com/a.png"/><rect fill="url(https://example.com/x)"/></svg>');
  assert.match(safe, /^<svg/);
  assert.doesNotMatch(safe, /onload|script|https:|url\(https:/i);
});
