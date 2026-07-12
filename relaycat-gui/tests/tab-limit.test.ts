import assert from "node:assert/strict";
import test from "node:test";

import { MAX_TAB_COUNT, canCreateTab } from "../src/tab-limit.ts";

test("allows creating a tab until the 24-tab limit is reached", () => {
  assert.equal(MAX_TAB_COUNT, 24);
  assert.equal(canCreateTab(23), true);
});

test("blocks creating a tab when 24 tabs already exist", () => {
  assert.equal(canCreateTab(24), false);
});
