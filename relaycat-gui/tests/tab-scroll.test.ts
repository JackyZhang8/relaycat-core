import assert from "node:assert/strict";
import test from "node:test";

import { tabScrollState } from "../src/tab-scroll.ts";

test("shows a right scroll control when tabs overflow to the right", () => {
  assert.deepEqual(
    tabScrollState({ scrollLeft: 0, clientWidth: 400, scrollWidth: 700 }),
    { overflow: true, canScrollLeft: false, canScrollRight: true },
  );
});

test("enables only the left scroll control at the end of the tab list", () => {
  assert.deepEqual(
    tabScrollState({ scrollLeft: 300, clientWidth: 400, scrollWidth: 700 }),
    { overflow: true, canScrollLeft: true, canScrollRight: false },
  );
});

test("hides scroll controls when every tab fits", () => {
  assert.deepEqual(
    tabScrollState({ scrollLeft: 0, clientWidth: 400, scrollWidth: 400 }),
    { overflow: false, canScrollLeft: false, canScrollRight: false },
  );
});
