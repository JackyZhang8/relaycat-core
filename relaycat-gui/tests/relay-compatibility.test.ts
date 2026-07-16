import assert from "node:assert/strict";
import test from "node:test";

import {
  createRelayCompatibilityChecker,
  type RelayCompatibilityCheck,
} from "../src/relay-compatibility.ts";
import { I18N } from "../src/i18n.ts";

test("caches only successful relay compatibility checks per trimmed URL", async () => {
  let calls = 0;
  const check = createRelayCompatibilityChecker(async (relayUrl) => {
    calls += 1;
    return { status: "compatible", reason: relayUrl };
  });

  assert.deepEqual(await check(" wss://relay.example.com/ws "), {
    status: "compatible",
    reason: "wss://relay.example.com/ws",
  });
  assert.deepEqual(await check("wss://relay.example.com/ws"), {
    status: "compatible",
    reason: "cached",
  });
  assert.equal(calls, 1);
});

test("does not cache incompatible or unverified relay compatibility checks", async () => {
  const outcomes: RelayCompatibilityCheck[] = [
    { status: "incompatible", action: "upgrade_gui", reason: "new protocol" },
    { status: "unverified", reason: "health endpoint unavailable" },
  ];
  let calls = 0;
  const check = createRelayCompatibilityChecker(async () => outcomes[calls++]);

  assert.equal((await check("wss://relay.example.com/ws")).status, "incompatible");
  assert.equal((await check("wss://relay.example.com/ws")).status, "unverified");
  assert.equal(calls, 2);
});

test("provides localized guidance for incompatible relay versions", () => {
  assert.equal(I18N.zh.relay_incompatible_title, "Relay 版本不兼容");
  assert.match(I18N.zh.relay_upgrade_gui, /升级 GUI/);
  assert.match(I18N.zh.relay_upgrade_server, /升级 relaycat-relay/);
  assert.equal(I18N.en.relay_incompatible_title, "Incompatible relay version");
  assert.match(I18N.en.relay_upgrade_gui, /newer RelayCat GUI/);
  assert.match(I18N.en.relay_upgrade_server, /relaycat-relay/);
});
