#!/usr/bin/env bun
//
// Sign the two Linux tarballs and merge them into architecture-specific
// Sparkle-format feeds. Waku's native Linux updater reads this same compact
// contract as the Windows updater; Sparkle itself is not involved.
//
// Usage:
//   bun scripts/appcast-linux.ts <assets-dir> <version>
//
// Env:
//   SPARKLE_PRIVATE_KEY        EdDSA private key, base64 (required)
//   WAKU_DOWNLOAD_URL_PREFIX   base URL for enclosure links
import { sign } from "node:crypto";
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";

import { defaultDownloadUrlPrefix } from "./appcast.ts";
import {
  appPublicKey,
  architectures,
  escapeXml,
  mergeItems,
  parseAppcast,
  privateKeyFromSparkleSecret,
  publicKeyBase64,
  type AppcastItem,
  type Architecture,
} from "./appcast-windows.ts";

export const appcastName = (arch: Architecture) => `appcast-linux-${arch}.xml`;

const targetTriple = (arch: Architecture) =>
  `${arch}-unknown-linux-gnu` as const;

export function renderAppcast(
  arch: Architecture,
  items: AppcastItem[],
): string {
  const entries = items
    .map(
      (item) => `    <item>
      <title>${escapeXml(item.version)}</title>
      <pubDate>${escapeXml(item.pubDate)}</pubDate>
      <sparkle:version>${escapeXml(item.version)}</sparkle:version>
      <sparkle:shortVersionString>${escapeXml(item.version)}</sparkle:shortVersionString>
      <enclosure url="${escapeXml(item.url)}" length="${item.length}" type="application/gzip" sparkle:edSignature="${escapeXml(item.signature)}" sparkle:os="linux" />
    </item>`,
    )
    .join("\n");
  return `<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
  <channel>
    <title>Waku (Linux ${arch})</title>
${entries}
  </channel>
</rss>
`;
}

export async function generateLinuxAppcasts(
  assetsDir: string,
  version: string,
  downloadUrlPrefix: string,
  pubDate: string,
): Promise<string[]> {
  const secret = process.env.SPARKLE_PRIVATE_KEY?.trim();
  if (!secret) {
    throw new Error("SPARKLE_PRIVATE_KEY is required to sign the Linux feed.");
  }
  const privateKey = privateKeyFromSparkleSecret(secret);
  const derived = publicKeyBase64(privateKey);
  const expected = await appPublicKey();
  if (derived !== expected) {
    throw new Error(
      `SPARKLE_PRIVATE_KEY does not match SUPublicEDKey (${expected}); ` +
        `it derives ${derived}. Signing with it would ship a feed the app rejects.`,
    );
  }

  const present = new Set(readdirSync(assetsDir));
  const written: string[] = [];
  for (const arch of architectures) {
    const archive = `waku-${version}-${targetTriple(arch)}.tar.gz`;
    if (!present.has(archive)) {
      console.warn(`No ${archive} in ${assetsDir}; leaving that feed alone.`);
      continue;
    }
    const path = join(assetsDir, archive);
    const bytes = new Uint8Array(await Bun.file(path).arrayBuffer());
    const item: AppcastItem = {
      version,
      url: `${downloadUrlPrefix}${archive}`,
      length: statSync(path).size,
      signature: Buffer.from(sign(null, bytes, privateKey)).toString("base64"),
      pubDate,
    };

    const feedPath = join(assetsDir, appcastName(arch));
    const previous = (await Bun.file(feedPath).exists())
      ? parseAppcast(await Bun.file(feedPath).text())
      : [];
    await Bun.write(feedPath, renderAppcast(arch, mergeItems(previous, [item])));
    written.push(feedPath);
    console.log(`Wrote ${feedPath} (${item.length} bytes signed)`);
  }
  if (written.length === 0) {
    throw new Error(`No waku-${version}-<target>.tar.gz found in ${assetsDir}`);
  }
  return written;
}

if (import.meta.main) {
  const [assetsDir, version] = process.argv.slice(2);
  if (!assetsDir || !version) {
    console.error("usage: bun scripts/appcast-linux.ts <assets-dir> <version>");
    process.exit(1);
  }
  await generateLinuxAppcasts(
    assetsDir,
    version,
    process.env.WAKU_DOWNLOAD_URL_PREFIX ?? defaultDownloadUrlPrefix,
    new Date().toUTCString(),
  );
}
