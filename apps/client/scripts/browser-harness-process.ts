import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

export function browserProfilePath(name: string): string {
  return resolve(tmpdir(), name);
}

interface HarnessProcess {
  readonly exitCode: number | null;
  readonly pid: number;
  kill: () => void;
}

export async function assertProcessRunning(
  subprocess: HarnessProcess,
  label: string,
): Promise<void> {
  await Bun.sleep(100);
  if (subprocess.exitCode !== null) throw new Error(`${label} process exited`);
}

export function stopProcessTree(subprocess: HarnessProcess): void {
  if (process.platform === "win32") {
    Bun.spawnSync(["taskkill", "/PID", String(subprocess.pid), "/T", "/F"]);
    return;
  }
  subprocess.kill();
}

export function removeBrowserProfile(path: string): void {
  try {
    rmSync(path, { recursive: true, force: true });
  } catch {
    // Chrome can briefly retain profile files while exiting; temp cleanup may retry later.
  }
}
