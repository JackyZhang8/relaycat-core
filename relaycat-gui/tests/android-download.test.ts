import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { androidQrHref, resolveAndroidDownload } from "../src/android-download.ts";

test("resolves Android version and HTTPS APK URL from the updater manifest", () => {
  assert.deepEqual(
    resolveAndroidDownload({
      latest_version_name: "0.1.7",
      apk: { url: "https://cdn.relaycat.cn/download/relaycat-app-0.1.7-android-arm64.apk" },
    }),
    {
      version: "0.1.7",
      url: "https://cdn.relaycat.cn/download/relaycat-app-0.1.7-android-arm64.apk",
    },
  );
});

test("rejects incomplete or unsafe Android updater manifests", () => {
  assert.equal(resolveAndroidDownload(null), null);
  assert.equal(resolveAndroidDownload({ latest_version_name: "0.1.7" }), null);
  assert.equal(
    resolveAndroidDownload({
      latest_version_name: "0.1.7",
      apk: { url: "http://cdn.relaycat.cn/download/relaycat.apk" },
    }),
    null,
  );
});

test("builds a QR image URL from the manifest APK URL", () => {
  const apkUrl = "https://cdn.relaycat.cn/download/relaycat-app-0.1.7-android-arm64.apk";
  assert.equal(
    androidQrHref(apkUrl),
    `https://api.qrserver.com/v1/create-qr-code/?size=216x216&data=${encodeURIComponent(apkUrl)}`,
  );
});

test("the landing-page Android QR target is keyboard-activatable", () => {
  const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  assert.match(html, /<button[^>]+id="lp-android-download"[^>]+type="button"/);
});
