// The Client's Tauri build script refuses to run without its bundled Guard
// resource. The real Guard is Windows-only, so `bun run verify` calls this on
// other hosts to stand in an empty file when none exists. An existing file is
// left alone; on Windows, build the real one with
// `bun run build:guard` in apps/client.
import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";

const releaseDir = `${import.meta.dir}/../target/release`;
const guardPath = `${releaseDir}/client-guard.exe`;

if (process.platform !== "win32" && !existsSync(guardPath)) {
  await mkdir(releaseDir, { recursive: true });
  await writeFile(guardPath, "");
  console.info(
    "Created empty stand-in target/release/client-guard.exe for checks",
  );
}
