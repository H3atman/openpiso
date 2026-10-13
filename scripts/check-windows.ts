// Windows compile check from Linux/macOS: clippy against the real MSVC target
// via cargo-xwin. Catches `cfg(windows)` code the host check never compiles.
// It never runs anything — Windows runtime behaviour still needs a Windows PC.
import { existsSync } from "node:fs";
import { mkdir, rm, writeFile } from "node:fs/promises";

const rootDir = `${import.meta.dir}/..`;
const target = "x86_64-pc-windows-msvc";

// Tauri build scripts refuse to run when bundle resources or frontend dists
// are missing. A check does not need the real artifacts, so stand in empty
// ones for whatever is absent and remove only what this script created.
const placeholderDirs = [
  "apps/web-admin/dist",
  "apps/admin/dist",
  "apps/client/dist",
];
const placeholderFiles = ["target/release/client-guard.exe"];

async function run(command: string[]): Promise<void> {
  console.info(`> ${command.join(" ")}`);
  const process = Bun.spawn(command, {
    cwd: rootDir,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  const exitCode = await process.exited;
  if (exitCode !== 0) {
    throw new Error(`Command failed (${exitCode}): ${command.join(" ")}`);
  }
}

async function createPlaceholders(): Promise<string[]> {
  const created: string[] = [];
  for (const dir of placeholderDirs) {
    if (!existsSync(`${rootDir}/${dir}`)) {
      await mkdir(`${rootDir}/${dir}`, { recursive: true });
      created.push(dir);
    }
  }
  for (const file of placeholderFiles) {
    if (!existsSync(`${rootDir}/${file}`)) {
      await mkdir(`${rootDir}/${file.slice(0, file.lastIndexOf("/"))}`, {
        recursive: true,
      });
      await writeFile(`${rootDir}/${file}`, "");
      created.push(file);
    }
  }
  return created;
}

if (
  Bun.spawnSync(["cargo", "xwin", "--version"], { cwd: rootDir }).exitCode !== 0
) {
  console.error(
    "cargo-xwin is missing. Install once:\n" +
      `  rustup target add ${target}\n` +
      "  cargo install --locked cargo-xwin\n" +
      "Also needs clang, lld-link and llvm-lib (LLVM) on PATH.",
  );
  process.exit(1);
}

const created = await createPlaceholders();
try {
  await run([
    "cargo",
    "xwin",
    "clippy",
    "--workspace",
    "--all-targets",
    "--target",
    target,
    "--",
    "-D",
    "warnings",
  ]);
  // Kiosk hardening is gated on cfg(all(windows, not(debug_assertions))).
  await run([
    "cargo",
    "xwin",
    "clippy",
    "--release",
    "-p",
    "openpiso-client",
    "--lib",
    "--target",
    target,
    "--",
    "-D",
    "warnings",
  ]);
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
} finally {
  for (const path of created) {
    await rm(`${rootDir}/${path}`, { recursive: true, force: true });
  }
}
