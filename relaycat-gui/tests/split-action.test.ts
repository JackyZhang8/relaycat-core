import assert from "node:assert/strict";
import test from "node:test";

import { runSplitAction } from "../src/split-action.ts";

test("starting a split closes the workspace before changing the terminal layout", () => {
  const actions: string[] = [];

  runSplitAction(
    () => actions.push("close-workspace"),
    () => actions.push("toggle-split"),
  );

  assert.deepEqual(actions, ["close-workspace", "toggle-split"]);
});
