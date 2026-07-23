import assert from "node:assert/strict";
import test from "node:test";

import { restoreTerminalFocusAfterOverlayClose } from "../src/pairing-focus.ts";

test("closing the pairing overlay restores terminal focus", () => {
  let focused = 0;

  const restored = restoreTerminalFocusAfterOverlayClose("ov-pair", () => {
    focused += 1;
  });

  assert.equal(restored, true);
  assert.equal(focused, 1);
});

test("closing another overlay does not take terminal focus", () => {
  let focused = 0;

  const restored = restoreTerminalFocusAfterOverlayClose("ov-new", () => {
    focused += 1;
  });

  assert.equal(restored, false);
  assert.equal(focused, 0);
});
