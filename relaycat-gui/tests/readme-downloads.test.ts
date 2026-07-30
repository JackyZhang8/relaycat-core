import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("the repository README points Android users to version 0.1.7", () => {
  const readme = readFileSync(new URL("../../README.md", import.meta.url), "utf8");
  const apkUrl =
    "https://cdn.relaycat.cn/download/relaycat-app-0.1.7-android-arm64.apk";
  const encodedApkUrl =
    "https%3A%2F%2Fcdn.relaycat.cn%2Fdownload%2Frelaycat-app-0.1.7-android-arm64.apk";

  assert.match(readme, /下载 Android 0\.1\.7 arm64 APK/);
  assert.ok(readme.includes(apkUrl), "README download link must use the 0.1.7 APK");
  assert.ok(readme.includes(encodedApkUrl), "README QR code must target the 0.1.7 APK");
  assert.doesNotMatch(readme, /relaycat-app-0\.1\.5-android-arm64\.apk/);
});
