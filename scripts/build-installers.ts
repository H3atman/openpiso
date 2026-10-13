import { mkdir, readdir, rm } from "node:fs/promises";

export const WINDOWS_TARGET = "x86_64-pc-windows-msvc";

type AppBundle = {
  productName: string;
  version: string;
  outputDir: "admin-web-admin" | "client";
};

export type InstallerBuildPlan = {
  bundleDir: string;
  tauriArgs: string[];
};

export function installerBuildPlan(
  platform: NodeJS.Platform,
): InstallerBuildPlan {
  if (platform === "win32") {
    return {
      bundleDir: "target/release/bundle/nsis",
      tauriArgs: ["build", "--bundles", "nsis"],
    };
  }

  if (platform === "linux" || platform === "darwin") {
    return {
      bundleDir: `target/${WINDOWS_TARGET}/release/bundle/nsis`,
      tauriArgs: [
        "build",
        "--runner",
        "cargo-xwin",
        "--target",
        WINDOWS_TARGET,
        "--bundles",
        "nsis",
      ],
    };
  }

  throw new Error(`Unsupported installer build host: ${platform}`);
}

export function tauriBuildCommand(
  appDir: "apps/admin" | "apps/client",
  plan: InstallerBuildPlan,
): string[] {
  const command = ["bun", "--cwd", appDir, "tauri", ...plan.tauriArgs];
  // Sideload builds do not need a signing key; CI uses the signed Admin config.
  if (appDir === "apps/admin") {
    command.push("--config", '{"bundle":{"createUpdaterArtifacts":false}}');
  }
  return command;
}

export function assertInstallerPrerequisites(
  platform: NodeJS.Platform,
  commandExists: (command: string) => boolean = (command) =>
    Bun.which(command) !== null,
): void {
  if (platform !== "win32" && !commandExists("makensis")) {
    throw new Error(
      "NSIS compiler `makensis` is missing. Install NSIS before cross-building " +
        "(Arch/CachyOS: `yay -S nsis`; Debian/Ubuntu: `sudo apt install nsis`; " +
        "macOS: `brew install nsis`).",
    );
  }
}

const rootDir = `${import.meta.dir}/..`;
const outputRoot = `${rootDir}/dist/installers`;

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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

async function appBundle(
  configPath: string,
  outputDir: AppBundle["outputDir"],
): Promise<AppBundle> {
  const config: unknown = await Bun.file(`${rootDir}/${configPath}`).json();
  if (
    !isRecord(config) ||
    typeof config.productName !== "string" ||
    typeof config.version !== "string"
  ) {
    throw new Error(`Invalid productName/version in ${configPath}`);
  }

  return {
    productName: config.productName,
    version: config.version,
    outputDir,
  };
}

async function copyInstallers(
  bundle: AppBundle,
  bundleDir: string,
): Promise<string[]> {
  const prefix = `${bundle.productName}_${bundle.version}_`;
  const installers = (await readdir(bundleDir)).filter(
    (name) => name.startsWith(prefix) && name.endsWith("-setup.exe"),
  );
  if (installers.length === 0) {
    throw new Error(
      `No NSIS installer found for ${bundle.productName} ${bundle.version}`,
    );
  }

  const destinationDir = `${outputRoot}/${bundle.outputDir}`;
  await mkdir(destinationDir, { recursive: true });

  return Promise.all(
    installers.map(async (name) => {
      const destination = `${destinationDir}/${name}`;
      await Bun.write(destination, Bun.file(`${bundleDir}/${name}`));
      return destination;
    }),
  );
}

async function main(): Promise<void> {
  const plan = installerBuildPlan(process.platform);
  assertInstallerPrerequisites(process.platform);
  const bundleDir = `${rootDir}/${plan.bundleDir}`;
  const bundles = await Promise.all([
    appBundle("apps/admin/src-tauri/tauri.conf.json", "admin-web-admin"),
    appBundle("apps/client/src-tauri/tauri.conf.json", "client"),
  ]);

  await rm(bundleDir, { recursive: true, force: true });
  console.info(
    // Bun loads OPENPISO_POSTHOG_TOKEN from `.env.local`; builds inherit it.
    process.env.OPENPISO_POSTHOG_TOKEN
      ? "Telemetry: ON (OPENPISO_POSTHOG_TOKEN set; Admin sends the Daily Summary)"
      : "Telemetry: off (no OPENPISO_POSTHOG_TOKEN; inert build)",
  );
  await run(["bun", "run", "build:web-admin"]);
  await run(tauriBuildCommand("apps/admin", plan));
  await run(tauriBuildCommand("apps/client", plan));

  await rm(outputRoot, { recursive: true, force: true });
  const outputs = (
    await Promise.all(
      bundles.map((bundle) => copyInstallers(bundle, bundleDir)),
    )
  ).flat();

  console.info("\nInstallers ready:");
  for (const output of outputs) {
    console.info(`- ${output.replace(`${rootDir}/`, "")}`);
  }
}

if (import.meta.main) {
  await main();
}
