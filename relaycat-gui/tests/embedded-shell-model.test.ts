import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_EMBEDDED_SHELLS_PER_SESSION,
  embeddedShellAction,
  embeddedShellCreateKind,
  nextLocalShellNumber,
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

test("each paired session has one shared Shell and four local slots", () => {
  assert.equal(MAX_EMBEDDED_SHELLS_PER_SESSION, 5);
  assert.equal(nextLocalShellNumber("relay-a", []), 2);
  assert.equal(
    nextLocalShellNumber("relay-a", [
      { ownerSessionId: "relay-a", number: 1 },
      { ownerSessionId: "relay-a", number: 2 },
      { ownerSessionId: "relay-b", number: 3 },
    ]),
    3,
  );
  assert.equal(
    nextLocalShellNumber("relay-a", [1, 2, 3, 4, 5].map((number) => ({
      ownerSessionId: "relay-a",
      number,
    }))),
    null,
  );
});

test("creation kind restores a missing shared Shell before using local slots", () => {
  const fourLocals = [2, 3, 4, 5].map((number) => ({
    ownerSessionId: "relay-a",
    kind: "local" as const,
    number,
  }));
  assert.equal(embeddedShellCreateKind("relay-a", fourLocals), "shared");
  assert.equal(
    embeddedShellCreateKind("relay-a", [
      { ownerSessionId: "relay-a", kind: "shared", number: 1 },
      ...fourLocals.slice(0, 3),
    ]),
    "local",
  );
  assert.equal(
    embeddedShellCreateKind("relay-a", [
      { ownerSessionId: "relay-a", kind: "shared", number: 1 },
      ...fourLocals,
    ]),
    "limit",
  );
});
