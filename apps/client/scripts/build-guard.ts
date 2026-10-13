import { copyFile, mkdir, readFile } from "node:fs/promises";
import { dirname } from "node:path";

export const WINDOWS_TARGET = "x86_64-pc-windows-msvc";
export const STAGED_GUARD_PATH = "target/release/client-guard.exe";

export type GuardBuildPlan = {
  command: string[];
  sourcePath: string;
  stagedPath: string;
};

export function guardBuildPlan(
  platform: NodeJS.Platform,
  targetTriple: string | undefined,
): GuardBuildPlan {
  if (platform === "win32") {
    return {
      command: ["cargo", "build", "-p", "client-guard", "--release"],
      sourcePath: STAGED_GUARD_PATH,
      stagedPath: STAGED_GUARD_PATH,
    };
  }

  if (targetTriple !== WINDOWS_TARGET) {
    throw new Error(
      `Client Guard requires the ${WINDOWS_TARGET} target on ${platform}. ` +
        "Build Windows installers with `bun run build:installers`.",
    );
  }

  return {
    command: [
      "cargo",
      "xwin",
      "build",
      "-p",
      "client-guard",
      "--release",
      "--target",
      WINDOWS_TARGET,
    ],
    sourcePath: `target/${WINDOWS_TARGET}/release/client-guard.exe`,
    stagedPath: STAGED_GUARD_PATH,
  };
}

async function run(command: string[], cwd: string): Promise<void> {
  console.info(`> ${command.join(" ")}`);
  const process = Bun.spawn(command, {
    cwd,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  const exitCode = await process.exited;
  if (exitCode !== 0) {
    throw new Error(`Command failed (${exitCode}): ${command.join(" ")}`);
  }
}

async function assertWindowsExecutable(path: string): Promise<void> {
  const bytes = await readFile(path);
  if (bytes[0] !== 0x4d || bytes[1] !== 0x5a) {
    throw new Error(`Client Guard is not a Windows executable: ${path}`);
  }
}

if (import.meta.main) {
  const rootDir = `${import.meta.dir}/../../..`;
  const plan = guardBuildPlan(
    process.platform,
    process.env.TAURI_ENV_TARGET_TRIPLE,
  );

  await run(plan.command, rootDir);

  const sourcePath = `${rootDir}/${plan.sourcePath}`;
  const stagedPath = `${rootDir}/${plan.stagedPath}`;
  await assertWindowsExecutable(sourcePath);

  if (sourcePath !== stagedPath) {
    await mkdir(dirname(stagedPath), { recursive: true });
    await copyFile(sourcePath, stagedPath);
    console.info(`Staged Client Guard at ${plan.stagedPath}`);
  }
}
