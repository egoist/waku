#!/usr/bin/env bun

import { $ } from "bun";
import {
  access,
  chmod,
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readlink,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { parseArgs } from "node:util";

const appName = "Waku";
const executableName = "Waku";
const packageName = "waku";
const defaultSigningIdentity = "GJE9R5VE87";
const defaultNotaryProfile = "NOTARY";
const projectRoot = resolve(import.meta.dir, "..");

const help = `Create a production macOS DMG for Waku.

Usage:
  bun scripts/package-dmg.ts [options]

Options:
  --output <path>               Output path (default: dist/Waku-<version>.dmg)
  --signing-identity <name>     Developer ID Application identity selector
                                (default: GJE9R5VE87; or WAKU_SIGNING_IDENTITY)
  --notary-profile <name>       notarytool keychain profile
                                (default: NOTARY; or WAKU_NOTARY_PROFILE)
  --build-number <number>       CFBundleVersion override
                                (or WAKU_BUILD_NUMBER)
  --volume-name <name>          Mounted DMG name (default: Waku)
  --skip-build                  Reuse target/release/waku
  --skip-notarize               Build a signed DMG without notarizing it
  --adhoc                       Ad-hoc sign and skip notarization (local testing)
  --help                        Show this help

Production example:
  bun scripts/package-dmg.ts

Before the first production build, create the keychain profile with:
  xcrun notarytool store-credentials NOTARY
`;

const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    adhoc: { type: "boolean" },
    "build-number": { type: "string" },
    help: { type: "boolean", short: "h" },
    "notary-profile": { type: "string" },
    output: { type: "string", short: "o" },
    "signing-identity": { type: "string" },
    "skip-build": { type: "boolean" },
    "skip-notarize": { type: "boolean" },
    "volume-name": { type: "string" },
  },
  strict: true,
});

if (values.help) {
  console.log(help);
  process.exit(0);
}

if (process.platform !== "darwin") {
  throw new Error("DMG packaging must run on macOS.");
}

function requireTool(name: string): void {
  if (!Bun.which(name)) {
    throw new Error(`Required tool not found in PATH: ${name}`);
  }
}

function logStep(message: string): void {
  console.log(`\n==> ${message}`);
}

type CargoMetadata = {
  packages: Array<{
    name: string;
    version: string;
  }>;
};

const adhoc = values.adhoc ?? false;
const skipNotarize = values["skip-notarize"] ?? false;
const configuredSigningIdentity =
  values["signing-identity"] ?? process.env.WAKU_SIGNING_IDENTITY;
const signingIdentity =
  configuredSigningIdentity ?? defaultSigningIdentity;
const notaryProfile =
  values["notary-profile"] ??
  process.env.WAKU_NOTARY_PROFILE ??
  defaultNotaryProfile;
const buildNumber =
  values["build-number"] ?? process.env.WAKU_BUILD_NUMBER;

if (adhoc && configuredSigningIdentity) {
  throw new Error("Use either --adhoc or --signing-identity, not both.");
}
if (buildNumber && !/^\d+(?:\.\d+){0,2}$/.test(buildNumber)) {
  throw new Error(
    "--build-number must contain one to three period-separated integers.",
  );
}

for (const tool of [
  "cargo",
  "codesign",
  "create-dmg",
  "diskutil",
  "plutil",
  "xattr",
]) {
  requireTool(tool);
}
if (!adhoc && !skipNotarize) {
  requireTool("xcrun");
  requireTool("spctl");
}

process.chdir(projectRoot);

const metadata = JSON.parse(
  await $`cargo metadata --no-deps --format-version 1`.quiet().text(),
) as CargoMetadata;
const cargoPackage = metadata.packages.find(
  (candidate) => candidate.name === packageName,
);
if (!cargoPackage) {
  throw new Error(`Cargo package "${packageName}" was not found.`);
}

const version = cargoPackage.version;
const shortVersion = version.split("-", 1)[0];
const outputPath = resolve(
  projectRoot,
  values.output ?? join("dist", `${appName}-${version}.dmg`),
);
const volumeName = values["volume-name"] ?? appName;
const releaseDirectory = resolve(
  projectRoot,
  process.env.CARGO_TARGET_DIR ?? "target",
  "release",
);
const releaseExecutable = join(releaseDirectory, packageName);
const appBundle = join(releaseDirectory, `${appName}.app`);
const contentsDirectory = join(appBundle, "Contents");

if (extname(outputPath).toLowerCase() !== ".dmg") {
  throw new Error(`Output path must end in .dmg: ${outputPath}`);
}
if (
  !volumeName.trim() ||
  volumeName.includes("/") ||
  volumeName.length > 27
) {
  throw new Error(
    "--volume-name must be non-empty, at most 27 characters, and cannot contain '/'.",
  );
}

let temporaryDirectory: string | undefined;
let mountedDmg = false;
let mountDirectory: string | undefined;

try {
  if (!values["skip-build"]) {
    logStep("Building the release executable");
    await $`cargo build --release`;
  }

  try {
    await access(releaseExecutable);
  } catch {
    throw new Error(
      `Release executable not found at ${releaseExecutable}. ` +
        "Run without --skip-build first.",
    );
  }

  logStep("Assembling the app bundle");
  await rm(appBundle, { force: true, recursive: true });
  await mkdir(join(contentsDirectory, "MacOS"), { recursive: true });
  await mkdir(join(contentsDirectory, "Resources"), { recursive: true });
  await copyFile(
    releaseExecutable,
    join(contentsDirectory, "MacOS", executableName),
  );
  await chmod(join(contentsDirectory, "MacOS", executableName), 0o755);
  await copyFile(
    join(projectRoot, "resources", "Info.plist"),
    join(contentsDirectory, "Info.plist"),
  );
  await $`plutil -replace CFBundleShortVersionString -string ${shortVersion} ${join(contentsDirectory, "Info.plist")}`;
  if (buildNumber) {
    await $`plutil -replace CFBundleVersion -string ${buildNumber} ${join(contentsDirectory, "Info.plist")}`;
  }
  await $`xattr -cr ${appBundle}`;

  const identity = adhoc ? "-" : signingIdentity!;
  logStep(adhoc ? "Ad-hoc signing the app" : `Signing the app as ${identity}`);
  if (adhoc) {
    await $`codesign --force --options runtime --sign - ${appBundle}`;
  } else {
    await $`codesign --force --options runtime --timestamp --sign ${identity} ${appBundle}`;
  }
  await $`codesign --verify --deep --strict --verbose=2 ${appBundle}`;

  temporaryDirectory = await mkdtemp(join(tmpdir(), "waku-dmg-"));
  const stagingDirectory = join(temporaryDirectory, "root");
  mountDirectory = join(temporaryDirectory, "mount");
  await mkdir(stagingDirectory);
  await cp(appBundle, join(stagingDirectory, `${appName}.app`), {
    preserveTimestamps: true,
    recursive: true,
  });
  await mkdir(dirname(outputPath), { recursive: true });
  await rm(outputPath, { force: true });

  logStep(`Creating the styled DMG at ${outputPath}`);
  await $`create-dmg --volname ${volumeName} --window-pos 200 120 --window-size 660 400 --text-size 13 --icon-size 128 --icon ${`${appName}.app`} 180 178 --hide-extension ${`${appName}.app`} --app-drop-link 480 178 --filesystem APFS --format ULFO --no-internet-enable --overwrite ${outputPath} ${stagingDirectory}`;

  logStep(adhoc ? "Ad-hoc signing the DMG" : "Signing the DMG");
  if (adhoc) {
    await $`codesign --force --sign - ${outputPath}`;
  } else {
    await $`codesign --force --timestamp --sign ${identity} ${outputPath}`;
  }
  await $`codesign --verify --verbose=2 ${outputPath}`;

  logStep("Verifying the DMG contents");
  await mkdir(mountDirectory);
  await $`diskutil image attach --readOnly --mountOptions nobrowse --mountPoint ${mountDirectory} ${outputPath}`;
  mountedDmg = true;
  await access(
    join(mountDirectory, `${appName}.app`, "Contents", "MacOS", executableName),
  );
  await access(join(mountDirectory, ".DS_Store"));
  const applicationsTarget = await readlink(
    join(mountDirectory, "Applications"),
  );
  if (applicationsTarget !== "/Applications") {
    throw new Error(
      `DMG Applications link points to "${applicationsTarget}", expected "/Applications".`,
    );
  }
  await $`codesign --verify --deep --strict --verbose=2 ${join(mountDirectory, `${appName}.app`)}`;
  await $`diskutil eject ${mountDirectory}`;
  mountedDmg = false;

  if (!adhoc && !skipNotarize) {
    logStep("Submitting the DMG for Apple notarization");
    const resultText =
      await $`xcrun notarytool submit ${outputPath} --keychain-profile ${notaryProfile!} --wait --output-format json`
        .quiet()
        .text();
    const result = JSON.parse(resultText) as {
      id?: string;
      message?: string;
      status?: string;
    };
    if (result.status !== "Accepted") {
      throw new Error(
        `Notarization ${result.status ?? "failed"}${result.id ? ` (${result.id})` : ""}: ` +
          (result.message ?? "inspect the submission with notarytool log"),
      );
    }
    console.log(`Notarization accepted: ${result.id ?? "unknown submission"}`);

    logStep("Stapling and assessing the notarized DMG");
    await $`xcrun stapler staple -v ${outputPath}`;
    await $`xcrun stapler validate -v ${outputPath}`;
    await $`spctl --assess --type open --context context:primary-signature --verbose=2 ${outputPath}`;
  } else if (adhoc) {
    console.warn(
      "\nCreated an ad-hoc signed DMG. It is suitable for local testing only.",
    );
  } else {
    console.warn(
      "\nCreated a Developer ID-signed DMG without notarization. " +
        "Gatekeeper will reject it on other Macs until it is notarized.",
    );
  }

  console.log(`\nDMG ready: ${outputPath}`);
} finally {
  if (mountedDmg && mountDirectory) {
    const result = await $`diskutil eject ${mountDirectory}`.quiet().nothrow();
    if (result.exitCode === 0) {
      mountedDmg = false;
    } else {
      console.warn(`Unable to detach temporary mount at ${mountDirectory}.`);
    }
  }
  if (temporaryDirectory && !mountedDmg) {
    await rm(temporaryDirectory, { force: true, recursive: true });
  } else if (temporaryDirectory) {
    console.warn(`Temporary files retained at ${temporaryDirectory}.`);
  }
}