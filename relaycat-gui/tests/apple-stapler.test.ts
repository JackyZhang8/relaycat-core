import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const script = new URL("../../scripts/apple-stapler.sh", import.meta.url);

function runStapler(failures: number, code: number) {
  const dir = mkdtempSync(join(tmpdir(), "relaycat-stapler-"));
  try {
    const state = join(dir, "attempts");
    writeFileSync(join(dir, "xcrun"), `#!/bin/bash
printf '%s\n' "$*" >> "$STAPLER_TEST_STATE"
count=$(wc -l < "$STAPLER_TEST_STATE")
if [ "$count" -le "$STAPLER_TEST_FAILURES" ]; then exit "$STAPLER_TEST_CODE"; fi
`, { mode: 0o755 });
    writeFileSync(join(dir, "sleep"), "#!/bin/bash\nexit 0\n", { mode: 0o755 });
    const result = spawnSync("bash", [script.pathname, "validate", "/tmp/Relay Cat.app"], {
      encoding: "utf8",
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}`,
        STAPLER_TEST_STATE: state, STAPLER_TEST_FAILURES: String(failures), STAPLER_TEST_CODE: String(code) },
    });
    const calls = result.status === 127 ? [] : readFileSync(state, "utf8").trim().split("\n");
    return { result, calls };
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

test("stapler retries service unavailable and preserves artifact paths", () => {
  const { result, calls } = runStapler(2, 68);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(calls.length, 3);
  assert.ok(calls.every(call => call === "stapler validate /tmp/Relay Cat.app"));
});

test("stapler stops after five unavailable responses", () => {
  const { result, calls } = runStapler(10, 68);
  assert.equal(result.status, 68, result.stderr);
  assert.equal(calls.length, 5);
});

test("stapler does not retry other validation failures", () => {
  const { result, calls } = runStapler(10, 65);
  assert.equal(result.status, 65, result.stderr);
  assert.equal(calls.length, 1);
});
