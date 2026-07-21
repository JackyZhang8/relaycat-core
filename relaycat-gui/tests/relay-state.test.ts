import assert from "node:assert/strict";
import test from "node:test";

import { shouldApplyRelaySnapshot } from "../src/relay-state.ts";

test("applies only a newer relay snapshot to a live tab", () => {
  assert.equal(shouldApplyRelaySnapshot(2, 3, false), true);
  assert.equal(shouldApplyRelaySnapshot(2, 2, false), false);
  assert.equal(shouldApplyRelaySnapshot(2, 1, false), false);
  assert.equal(shouldApplyRelaySnapshot(2, 3, true), false);
});
