import assert from "node:assert/strict";
import test from "node:test";

import {
  embeddedShellAction,
  embeddedShellHeight,
  storedEmbeddedShellHeight,
  toggleTermSidePanel,
} from "../src/embedded-shell-model.ts";

const shells = [
  { id: "embedded-shell-1", project: "/work/one" },
  { id: "embedded-shell-2", project: "/work/two" },
];

test("selects an existing project shell or creates a new one", () => {
  assert.deepEqual(embeddedShellAction(false, null, "/work/one", shells), {
    kind: "select",
    id: "embedded-shell-1",
  });
  assert.deepEqual(embeddedShellAction(false, null, "/work/new", shells), {
    kind: "create",
    project: "/work/new",
  });
});

test("hides only when the open dock already shows the requested project", () => {
  assert.deepEqual(
    embeddedShellAction(true, "embedded-shell-1", "/work/one", shells),
    { kind: "hide" },
  );
  assert.deepEqual(
    embeddedShellAction(true, "embedded-shell-1", "/work/two", shells),
    { kind: "select", id: "embedded-shell-2" },
  );
  assert.deepEqual(
    embeddedShellAction(false, "embedded-shell-2", "/work/two", shells),
    { kind: "select", id: "embedded-shell-2" },
  );
});

test("requires a project and clamps dock height", () => {
  assert.deepEqual(embeddedShellAction(false, null, null, shells), { kind: "disabled" });
  assert.equal(embeddedShellHeight(80, 900), 160);
  assert.equal(embeddedShellHeight(320, 900), 320);
  assert.equal(embeddedShellHeight(800, 900), 540);
  assert.equal(storedEmbeddedShellHeight(null, 900), null);
  assert.equal(storedEmbeddedShellHeight("320", 900), 320);
});

test("file and Git toggles affect only the requested session state", () => {
  assert.equal(toggleTermSidePanel(null, "files"), "files");
  assert.equal(toggleTermSidePanel("files", "files"), null);
  assert.equal(toggleTermSidePanel("files", "git"), "git");
  assert.equal(toggleTermSidePanel("git", "git"), null);
  assert.equal(toggleTermSidePanel("history", "git"), null);
});
