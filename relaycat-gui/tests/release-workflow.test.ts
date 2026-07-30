import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const workflowPath = new URL("../../.github/workflows/release-gui.yml", import.meta.url);
const macUpdaterScriptPath = new URL("../scripts/prepare-macos-updater.sh", import.meta.url);

test("the GUI release rebuilds the macOS updater before collecting artifacts", () => {
  const workflow = readFileSync(workflowPath, "utf8");
  const buildStep = workflow.indexOf("- name: Build GUI bundles");
  const prepareStep = workflow.indexOf("- name: Prepare signed macOS updater");
  const collectStep = workflow.indexOf("- name: Collect GUI artifacts");

  assert.ok(buildStep >= 0, "missing GUI build step");
  assert.ok(prepareStep > buildStep, "macOS updater preparation must follow the GUI build");
  assert.ok(collectStep > prepareStep, "artifact collection must use the rebuilt macOS updater");
  assert.match(workflow, /bash scripts\/prepare-macos-updater\.sh "\$app_path" "\$updater_path"/);
});

test("the macOS updater script rebuilds and validates a distributable archive", () => {
  assert.ok(existsSync(macUpdaterScriptPath), "missing prepare-macos-updater.sh");
  const script = readFileSync(macUpdaterScriptPath, "utf8");

  assert.match(script, /set -euo pipefail/);
  assert.match(script, /xattr -cr/);
  assert.match(script, /codesign[\s\S]*--options runtime[\s\S]*--timestamp/);
  assert.match(script, /xcrun notarytool submit[\s\S]*--wait/);
  assert.match(script, /xcrun stapler staple/);
  assert.match(script, /COPYFILE_DISABLE=1 tar -czf/);
  assert.match(script, /npx tauri signer sign/);
  assert.match(script, /\[ ! -s "\$\{updater_archive\}\.sig" \]/);
  assert.match(script, /codesign --verify --deep --strict/);
  assert.match(script, /spctl --assess --type execute/);
});
