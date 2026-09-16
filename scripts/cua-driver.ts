// Pinned SDK artifacts, shared by every packager and the dev watcher.
// Only libraries/support files are bundled; Waku never runs cua-driver serve.
import { $ } from "bun";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { writeCuaSkill } from "./cua-api";
import { prepareCuaHost } from "./cua-host";

export const cuaVersion = "0.28.0";
const root = resolve(import.meta.dir, "..");
const artifacts = {
  "darwin-arm64": [
    "82ea1801a5a800b2e46199e8b88694e8bba5670fd533c63a32766c753300d4d6",
    "libcua_driver_sdk.dylib",
  ],
  "darwin-x86_64": [
    "d758a7d45df89dbf849f01011d6e88591449face9277707d9dc749345343442d",
    "libcua_driver_sdk.dylib",
  ],
  "linux-arm64": [
    "984b0932b18f1fe1fdf738d3b7e07c1581da3ffc45b62080507beeeeb5e1b5a2",
    "libcua_driver_sdk.so",
  ],
  "linux-x86_64": [
    "b3029d0e1d462bc832dd689a0bdee92852a50865dafb987318f2e334ec4eed4e",
    "libcua_driver_sdk.so",
  ],
  "windows-arm64": [
    "86f9af260d64693f61736193891cdf65bfdfc010df0f91491f3ba8fbd38de498",
    "cua_driver_sdk.dll",
  ],
  "windows-x86_64": [
    "dbae495361a3d5862f7ed57cb82dc7a83f46e7a9ab111a6a23a8acaa79ec6265",
    "cua_driver_sdk.dll",
  ],
} as const;

export function cuaPlatform(
  platform = process.platform,
  arch = process.arch,
): keyof typeof artifacts {
  const os = platform === "win32" ? "windows" : platform;
  const cpu = arch === "x64" ? "x86_64" : arch;
  const key = `${os}-${cpu}`;
  if (!(key in artifacts))
    throw new Error(`Cua Driver does not ship an SDK for ${key}`);
  return key as keyof typeof artifacts;
}

export async function prepareCuaSdk(platform = cuaPlatform()): Promise<string> {
  const [checksum, library] = artifacts[platform];
  const cacheRoot = join(root, ".waku-cache", "cua-driver", cuaVersion);
  const destination = join(cacheRoot, platform);
  const support = platform.startsWith("windows")
    ? "cua-driver-uia.exe"
    : "cua_driver_abi.h";
  if (
    existsSync(join(destination, library)) &&
    existsSync(join(destination, support))
  )
    return destination;
  await mkdir(cacheRoot, { recursive: true });
  const staging = await mkdtemp(join(cacheRoot, ".download-"));
  try {
    const name = `cua-driver-rs-${cuaVersion}-${platform}`;
    const extension = platform.startsWith("windows") ? "zip" : "tar.gz";
    const archive = join(staging, `sdk.${extension}`);
    const response = await fetch(
      `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${cuaVersion}/${name}.${extension}`,
    );
    if (!response.ok)
      throw new Error(`Cua SDK download failed: ${response.status}`);
    const data = Buffer.from(await response.arrayBuffer());
    if (createHash("sha256").update(data).digest("hex") !== checksum)
      throw new Error("Cua SDK checksum mismatch");
    await writeFile(archive, data);
    // bsdtar ships with macOS and Windows; GNU tar handles the Linux archives.
    await $`tar -xf ${archive} -C ${staging}`.quiet();
    const unpacked = join(staging, name);
    for (const file of [library, "cua_driver_abi.h", support]) {
      if (!existsSync(join(unpacked, file)))
        throw new Error(`Cua SDK archive is missing ${file}`);
    }
    await rename(unpacked, destination);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
  return destination;
}

export async function bundleComputerUse(
  binDirectory: string,
  resourcesDirectory: string,
  profile: "debug" | "release",
): Promise<void> {
  const platform = cuaPlatform();
  const sdk = await prepareCuaSdk(platform);
  const hostSdk = await prepareCuaHost();
  const target = resolve(
    root,
    process.env.CARGO_TARGET_DIR || "target",
    profile,
  );
  const suffix = process.platform === "win32" ? ".exe" : "";
  await mkdir(binDirectory, { recursive: true });
  await mkdir(resourcesDirectory, { recursive: true });
  for (const file of [`waku_js_repl${suffix}`, `waku_computer_use${suffix}`]) {
    const destination = join(binDirectory, file);
    if (resolve(target, file) !== resolve(destination))
      await cp(join(target, file), destination);
  }
  await cp(
    join(hostSdk, artifacts[platform][1]),
    join(binDirectory, artifacts[platform][1]),
  );
  if (process.platform === "win32")
    await cp(
      join(sdk, "cua-driver-uia.exe"),
      join(binDirectory, "cua-driver-uia.exe"),
    );
  if (process.platform === "linux") {
    await cp(
      join(sdk, "wayland-helper"),
      join(resourcesDirectory, "computer-use", "wayland-helper"),
      { recursive: true },
    );
  }
  for (const [source, relative] of [
    ["resources/computer-use/pi-extension.ts", "computer-use/pi-extension.ts"],
    ["resources/computer-use/CUA-LICENSE", "computer-use/CUA-LICENSE"],
  ]) {
    const destination = join(resourcesDirectory, relative!);
    await mkdir(dirname(destination), { recursive: true });
    await cp(join(root, source!), destination);
  }
  await writeCuaSkill(
    join(binDirectory, `waku_computer_use${suffix}`),
    join(resourcesDirectory, "skills/waku-computer-use/SKILL.md"),
  );
}

if (import.meta.main) {
  const [command, ...args] = process.argv.slice(2);
  if (command === "prepare") console.log(await prepareCuaSdk());
  else if (
    command === "bundle" &&
    args.length === 3 &&
    ["debug", "release"].includes(args[2]!)
  ) {
    await bundleComputerUse(
      resolve(args[0]!),
      resolve(args[1]!),
      args[2] as "debug" | "release",
    );
  } else
    throw new Error(
      "Usage: bun scripts/cua-driver.ts prepare | bundle <bin-dir> <resources-dir> <debug|release>",
    );
}
