#!/usr/bin/env bun
//
// Sign update archives and (re)generate the Sparkle appcast for a directory.
//
// Usage:
//   bun scripts/appcast.ts <updates-dir>
//
// <updates-dir> holds the packaged archives (e.g. Waku-0.2.0.zip) plus any
// older archives so Sparkle can build binary deltas. appcast.xml is written
// into that directory. The private EdDSA key is read from SPARKLE_PRIVATE_KEY
// when set, otherwise from the login keychain (see RELEASING.md).
//
// Env overrides:
//   SPARKLE_BIN                dir containing the Sparkle tools
//   SPARKLE_PRIVATE_KEY        EdDSA private key (CI; otherwise the keychain)
//   WAKU_DOWNLOAD_URL_PREFIX   base URL for enclosure links
import { $ } from "bun";
import { existsSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

const projectRoot = resolve(import.meta.dir, "..");

export const defaultDownloadUrlPrefix = "https://releases.waku.sh/";

/** Locate Sparkle's `generate_appcast`: SPARKLE_BIN first, then the pinned
 *  distribution scripts/bundle.sh caches under .waku-cache, then PATH. */
export function findGenerateAppcast(): string | null {
  const fromEnv = process.env.SPARKLE_BIN;
  if (fromEnv) {
    const candidate = join(fromEnv, "generate_appcast");
    if (existsSync(candidate)) return candidate;
  }

  const cacheRoot = join(projectRoot, ".waku-cache", "sparkle");
  if (existsSync(cacheRoot)) {
    const versionOrder = new Intl.Collator("en", { numeric: true });
    const versions = readdirSync(cacheRoot)
      .filter((name) => !name.startsWith("."))
      .sort((a, b) => versionOrder.compare(b, a));
    for (const version of versions) {
      const candidate = join(cacheRoot, version, "bin", "generate_appcast");
      if (existsSync(candidate)) return candidate;
    }
  }

  return Bun.which("generate_appcast");
}

/** Sign the archives in `updatesDir` and (re)write appcast.xml. */
export async function generateAppcast(
  updatesDir: string,
  downloadUrlPrefix: string,
): Promise<void> {
  const generator = findGenerateAppcast();
  if (!generator) {
    throw new Error(
      "generate_appcast not found. Run scripts/bundle.sh once to populate " +
        ".waku-cache/sparkle, or set SPARKLE_BIN to a Sparkle tools bin/ dir.",
    );
  }
  console.log(`Using: ${generator}`);
  // Same prefix for both: archives and the Waku-<version>.md release notes are
  // served from the same origin. The notes prefix makes generate_appcast emit
  // <sparkle:releaseNotesLink> for any notes file matching an archive name.
  const privateKey = process.env.SPARKLE_PRIVATE_KEY?.trim();
  const command = [
    generator,
    "--download-url-prefix",
    downloadUrlPrefix,
    "--release-notes-url-prefix",
    downloadUrlPrefix,
    ...(privateKey ? ["--ed-key-file", "-"] : []),
    updatesDir,
  ];
  if (privateKey) {
    await $`${command}`.stdin(privateKey);
  } else {
    await $`${command}`;
  }
  console.log(`Wrote ${join(updatesDir, "appcast.xml")}`);
}

if (import.meta.main) {
  const updatesDir = process.argv[2];
  if (!updatesDir) {
    console.error("usage: bun scripts/appcast.ts <updates-dir>");
    process.exit(1);
  }
  const prefix =
    process.env.WAKU_DOWNLOAD_URL_PREFIX ?? defaultDownloadUrlPrefix;
  await generateAppcast(updatesDir, prefix);
}
