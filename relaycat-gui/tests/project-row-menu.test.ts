import assert from "node:assert/strict";
import test from "node:test";

import { projectRowMenuItems } from "../src/project-row-menu.ts";

test("project row menu exposes copy and fill-path actions in order", () => {
  const items = projectRowMenuItems("/work/relaycat", {
    copy: () => {},
    fill: () => {},
  });

  assert.deepEqual(
    items.map(({ labelKey }) => labelKey),
    ["copy", "proj_fill_path"],
  );
});

test("project row menu actions use the path from the right-clicked row", async () => {
  const calls: string[] = [];
  const items = projectRowMenuItems("/work/relaycat", {
    copy: async (path) => calls.push(`copy:${path}`),
    fill: (path) => calls.push(`fill:${path}`),
  });

  await items[0].run();
  await items[1].run();

  assert.deepEqual(calls, ["copy:/work/relaycat", "fill:/work/relaycat"]);
});
