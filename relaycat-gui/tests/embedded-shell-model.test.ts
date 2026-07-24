import assert from "node:assert/strict";
import test from "node:test";

import {
  embeddedShellAction,
  toggleTermSidePanel,
} from "../src/embedded-shell-model.ts";

const shells = [
  { id: "embedded-shell-1", project: "/work/one" },
  { id: "embedded-shell-2", project: "/work/two" },
];

test("selects an existing project shell or creates a new one", () => {
  assert.deepEqual(embeddedShellAction("/work/one", shells), {
    kind: "select",
    id: "embedded-shell-1",
  });
  assert.deepEqual(embeddedShellAction("/work/new", shells), {
    kind: "create",
    project: "/work/new",
  });
});

test("requires a project before activating a Shell", () => {
  assert.deepEqual(embeddedShellAction(null, shells), { kind: "disabled" });
});

test("file Git and Shell entries are mutually exclusive per session", () => {
  assert.equal(toggleTermSidePanel(null, "files"), "files");
  assert.equal(toggleTermSidePanel("files", "files"), null);
  assert.equal(toggleTermSidePanel("files", "git"), "git");
  assert.equal(toggleTermSidePanel("git", "git"), null);
  assert.equal(toggleTermSidePanel("history", "git"), null);
  assert.equal(toggleTermSidePanel("git", "shell"), "shell");
  assert.equal(toggleTermSidePanel("shell", "shell"), null);
  assert.equal(toggleTermSidePanel("shell", "files"), "files");
});
