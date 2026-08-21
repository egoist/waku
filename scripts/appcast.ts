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
  // The key is fed on stdin (`--ed-key-file -`) so it never lands on disk.
  // Without one, generate_appcast reads the login keychain instead and wants
  // nothing on stdin, so that path just leaves the terminal's attached.
  const child = Bun.spawn(command, {
    stdin: privateKey ? new Blob([`${privateKey}\n`]) : "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  const exitCode = await child.exited;
  if (exitCode !== 0) {
    throw new Error(`generate_appcast exited with ${exitCode}`);
  }

  // generate_appcast exits 0 after writing an *unsigned* feed when the key it
  // used does not match the bundle's SUPublicEDKey, and Sparkle rejects an
  // unsigned enclosure — so a silent mismatch would ship a dead update feed.
  const appcastPath = join(updatesDir, "appcast.xml");
  const unsigned = [
    ...(await Bun.file(appcastPath).text()).matchAll(/<enclosure\b[^>]*>/g),
  ]
    .filter(([tag]) => !tag.includes("sparkle:edSignature="))
    .map(([tag]) => tag.match(/url="([^"]*)"/)?.[1] ?? tag);
  if (unsigned.length > 0) {
    throw new Error(
      `generate_appcast left ${unsigned.length} enclosure(s) unsigned: ` +
        `${unsigned.join(", ")}. The signing key does not match the app's ` +
        "SUPublicEDKey (check SPARKLE_PRIVATE_KEY, or the login keychain).",
    );
  }
  console.log(`Wrote ${appcastPath}`);
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
