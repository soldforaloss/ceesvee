// Build the Tauri updater manifest (`latest.json`) deterministically from the
// per-bundle `.sig` files of a release.
//
// The release workflow builds each platform in parallel and uploads its signed
// bundles, but lets a single final job assemble `latest.json` — so the manifest
// is written exactly once (no cross-job upload race) and always contains every
// platform whose signature is present.
//
// Usage: node scripts/build-latest-json.mjs <tag> <owner/repo> <sigsDir> <outFile>
//
// Note: the platform keys below assume x86_64 Windows/Linux plus a universal
// macOS build (which covers both Apple-silicon and Intel). If arm64 Windows or
// Linux targets are ever added, extend `keysFor`.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [, , tag, repo, sigsDir, outFile] = process.argv;
if (!tag || !repo || !sigsDir || !outFile) {
  console.error("usage: build-latest-json.mjs <tag> <owner/repo> <sigsDir> <outFile>");
  process.exit(1);
}

const version = tag.replace(/^v/, "");
const base = `https://github.com/${repo}/releases/download/${tag}/`;

/** Map a `.sig` filename to the updater platform keys it satisfies. */
function keysFor(name) {
  if (name.endsWith(".AppImage.sig")) return ["linux-x86_64", "linux-x86_64-appimage"];
  if (name.endsWith(".deb.sig")) return ["linux-x86_64-deb"];
  if (name.endsWith("-setup.exe.sig")) return ["windows-x86_64-nsis"];
  if (name.endsWith(".msi.sig")) return ["windows-x86_64", "windows-x86_64-msi"];
  if (name.endsWith(".app.tar.gz.sig"))
    return ["darwin-aarch64", "darwin-x86_64", "darwin-aarch64-app", "darwin-x86_64-app"];
  return [];
}

const platforms = {};
for (const sig of readdirSync(sigsDir).filter((f) => f.endsWith(".sig"))) {
  const keys = keysFor(sig);
  if (keys.length === 0) continue;
  const asset = sig.slice(0, -".sig".length);
  const signature = readFileSync(join(sigsDir, sig), "utf8").trim();
  const url = base + asset;
  for (const key of keys) platforms[key] = { signature, url };
}

if (Object.keys(platforms).length === 0) {
  console.error(`no updater signatures (*.sig) found in ${sigsDir}`);
  process.exit(1);
}

const manifest = {
  version,
  notes: "See the assets below to download and install this version.",
  pub_date: new Date().toISOString(),
  platforms,
};

writeFileSync(outFile, JSON.stringify(manifest, null, 2));
console.error(
  `wrote ${outFile} for ${version} with platforms: ${Object.keys(platforms).join(", ")}`,
);
