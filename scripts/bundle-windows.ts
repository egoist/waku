#!/usr/bin/env bun
//
// Build and package the Windows release: a portable zip and the Inno Setup
// installer the in-app updater re-runs silently. Mirrors bundle-linux.sh for
// the archive half and resources/windows/waku.iss for the installer half.
//
// Usage:
//   bun scripts/bundle-windows.ts
//
// Env:
//   CARGO_TARGET_DIR              cargo target directory (default: target)
//   WINDOWS_CERTIFICATE           base64 Authenticode .pfx (optional)
//   WINDOWS_CERTIFICATE_PASSWORD  password for it
import { $ } from "bun";
import { existsSync, readdirSync, statSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const packageName = "waku";
const projectRoot = resolve(import.meta.dir, "..");

/** The updater picks its feed by Rust arch name, so the installer carries
 *  that rather than the full triple. */
const architectureForTarget: Record<string, string> = {
  "x86_64-pc-windows-msvc": "x86_64",
  "aarch64-pc-windows-msvc": "aarch64",
};

interface CargoMetadata {
  packages: { name: string; version: string }[];
}

/** Inno Setup's compiler, however it was installed. */
function findInnoSetupCompiler(): string {
  const onPath = Bun.which("ISCC.exe") ?? Bun.which("iscc");
  if (onPath) return onPath;
  for (const base of [process.env.ProgramFiles, process.env["ProgramFiles(x86)"]]) {
    if (!base) continue;
    const candidate = join(base, "Inno Setup 6", "ISCC.exe");
    if (existsSync(candidate)) return candidate;
  }
  throw new Error(
    "ISCC.exe was not found. Install Inno Setup 6 (choco install innosetup).",
  );
}

/** The newest signtool in the installed Windows SDKs, preferring the host's
 *  own architecture. An SDK lays these out as `bin\<version>\<arch>`, with
 *  older ones dropping the version directory. */
function findSigntool(): string {
  const root = join(
    process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)",
    "Windows Kits",
    "10",
    "bin",
  );
  if (!existsSync(root)) {
    throw new Error(`signtool.exe was not found: ${root} does not exist.`);
  }
  const versionOrder = new Intl.Collator("en", { numeric: true });
  const versions = readdirSync(root).sort((a, b) => versionOrder.compare(b, a));
  // arm64 Windows runs the x64 tool under emulation, so it stays as a
  // fallback rather than a failure.
  const architectures =
    process.arch === "arm64" ? ["arm64", "x64", "x86"] : ["x64", "x86"];
  for (const architecture of architectures) {
    for (const directory of [...versions.map((v) => join(root, v)), root]) {
      const candidate = join(directory, architecture, "signtool.exe");
      if (existsSync(candidate)) return candidate;
    }
  }
  throw new Error(`signtool.exe was not found under ${root}.`);
}

async function sign(
  signtool: string,
  certificate: string,
  password: string,
  files: string[],
): Promise<void> {
  for (const file of files) {
    await $`${signtool} sign /f ${certificate} /p ${password} /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 ${file}`;
  }
}

process.chdir(projectRoot);

if (process.platform !== "win32") {
  throw new Error(
    "bundle-windows.ts builds a native Windows release and must run on Windows.",
  );
}

const targetDirectory = resolve(process.env.CARGO_TARGET_DIR || "target");
const releaseDirectory = join(targetDirectory, "release");

const metadata = JSON.parse(
  await $`cargo metadata --no-deps --format-version 1`.quiet().text(),
) as CargoMetadata;
const version = metadata.packages.find(
  (candidate) => candidate.name === packageName,
)?.version;
if (!version) {
  throw new Error(`Cargo package "${packageName}" was not found.`);
}

const hostLine = (await $`rustc -vV`.quiet().text())
  .split("\n")
  .find((line) => line.startsWith("host: "));
const targetTriple = hostLine?.slice("host: ".length).trim();
const architecture = targetTriple
  ? architectureForTarget[targetTriple]
  : undefined;
if (!targetTriple || !architecture) {
  throw new Error(`Unsupported Windows target ${targetTriple ?? "(unknown)"}`);
}

const packageDirectoryName = `waku-${version}-${targetTriple}`;
const archive = join(releaseDirectory, `${packageDirectoryName}.zip`);
const installer = join(
  releaseDirectory,
  `Waku-${version}-${architecture}-Setup.exe`,
);

await $`cargo build --locked --release --package waku --bin waku --package waku-daemon --bin waku-daemon`;

const staging = await mkdtemp(join(tmpdir(), "waku-bundle-"));
try {
  // Both executables stay side by side: the app resolves the daemon next to
  // itself, so the layout is what makes an extracted zip runnable in place.
  const packageDirectory = join(staging, packageDirectoryName);
  await mkdir(packageDirectory, { recursive: true });
  for (const file of ["waku.exe", "waku-daemon.exe"]) {
    await copyFile(join(releaseDirectory, file), join(packageDirectory, file));
  }
  await copyFile(join(projectRoot, "LICENSE"), join(packageDirectory, "LICENSE"));

  // Authenticode has to be applied before anything is packaged, so the
  // executables inside the zip and the installer are all signed. Unsigned
  // builds still package, so a fork without a certificate can release.
  const certificateData = process.env.WINDOWS_CERTIFICATE;
  const certificatePassword = process.env.WINDOWS_CERTIFICATE_PASSWORD;
  let certificate: string | undefined;
  let signtool: string | undefined;
  if (certificateData && certificatePassword) {
    certificate = join(staging, "certificate.pfx");
    await writeFile(certificate, Buffer.from(certificateData, "base64"));
    signtool = findSigntool();
    await sign(signtool, certificate, certificatePassword, [
      join(packageDirectory, "waku.exe"),
      join(packageDirectory, "waku-daemon.exe"),
    ]);
  } else {
    console.log("No WINDOWS_CERTIFICATE set; packaging unsigned binaries.");
  }

  await mkdir(releaseDirectory, { recursive: true });
  await rm(archive, { force: true });
  // Windows 10 1803 and later ship bsdtar, which writes a zip when the output
  // name says so — no PowerShell, and the same one-versioned-directory layout
  // the Linux tarball uses.
  await $`tar -a -c -f ${archive} -C ${staging} ${packageDirectoryName}`;
  console.log(`Created ${archive}`);

  // The installer is what the in-app updater downloads and re-runs, so it
  // ships from the same signed staging directory as the zip.
  await rm(installer, { force: true });
  await $`${findInnoSetupCompiler()} ${`/DAppVersion=${version}`} ${`/DArch=${architecture}`} ${`/DStageDir=${packageDirectory}`} ${`/DOutputDir=${releaseDirectory}`} ${join(projectRoot, "resources", "windows", "waku.iss")}`;
  if (!existsSync(installer)) {
    throw new Error(`ISCC did not produce ${installer}`);
  }
  if (certificate && signtool && certificatePassword) {
    await sign(signtool, certificate, certificatePassword, [installer]);
  }
  console.log(`Created ${installer} (${statSync(installer).size} bytes)`);
} finally {
  await rm(staging, { recursive: true, force: true });
}
