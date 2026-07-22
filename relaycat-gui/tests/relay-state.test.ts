import assert from "node:assert/strict";
import test from "node:test";

import { I18N } from "../src/i18n.ts";
import { shouldApplyRelaySnapshot } from "../src/relay-state.ts";

test("applies only a newer relay snapshot to a live tab", () => {
  assert.equal(shouldApplyRelaySnapshot(2, 3, false), true);
  assert.equal(shouldApplyRelaySnapshot(2, 2, false), false);
  assert.equal(shouldApplyRelaySnapshot(2, 1, false), false);
  assert.equal(shouldApplyRelaySnapshot(2, 3, true), false);
});

test("shows paired status without a device count", () => {
  assert.equal(I18N.zh.paired_devices, "已配对");
  assert.equal(I18N.en.paired_devices, "Paired");
});
