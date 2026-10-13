import { describe, expect, test } from "bun:test";

import {
  assertInstallerPrerequisites,
  installerBuildPlan,
  tauriBuildCommand,
  WINDOWS_TARGET,
} from "./build-installers";

describe("installerBuildPlan", () => {
  test("preserves native Windows build commands and bundle path", () => {
    const plan = installerBuildPlan("win32");

    expect(plan).toEqual({
      bundleDir: "target/release/bundle/nsis",
      tauriArgs: ["build", "--bundles", "nsis"],
    });
    expect(tauriBuildCommand("apps/client", plan)).toEqual([
      "bun",
      "--cwd",
      "apps/client",
      "tauri",
      "build",
      "--bundles",
      "nsis",
    ]);
  });

  test.each(["linux", "darwin"] as const)(
    "cross-builds both Windows installers from %s",
    (platform) => {
      const plan = installerBuildPlan(platform);
      const expectedArgs = [
        "build",
        "--runner",
        "cargo-xwin",
        "--target",
        WINDOWS_TARGET,
        "--bundles",
        "nsis",
      ];

      expect(plan).toEqual({
        bundleDir: `target/${WINDOWS_TARGET}/release/bundle/nsis`,
        tauriArgs: expectedArgs,
      });
      expect(tauriBuildCommand("apps/admin", plan)).toEqual([
        "bun",
        "--cwd",
        "apps/admin",
        "tauri",
        ...expectedArgs,
        "--config",
        '{"bundle":{"createUpdaterArtifacts":false}}',
      ]);
      expect(tauriBuildCommand("apps/client", plan)).toEqual([
        "bun",
        "--cwd",
        "apps/client",
        "tauri",
        ...expectedArgs,
      ]);
    },
  );

  test("keeps offline Admin installers independent of signing keys on Windows", () => {
    expect(
      tauriBuildCommand("apps/admin", installerBuildPlan("win32")),
    ).toEqual([
      "bun",
      "--cwd",
      "apps/admin",
      "tauri",
      "build",
      "--bundles",
      "nsis",
      "--config",
      '{"bundle":{"createUpdaterArtifacts":false}}',
    ]);
  });

  test("rejects unsupported build hosts", () => {
    expect(() => installerBuildPlan("aix")).toThrow(
      "Unsupported installer build host: aix",
    );
  });
});

describe("assertInstallerPrerequisites", () => {
  test("requires NSIS before a cross-build starts", () => {
    expect(() => assertInstallerPrerequisites("linux", () => false)).toThrow(
      "NSIS compiler `makensis` is missing",
    );
  });

  test("accepts installed NSIS and native Windows builds", () => {
    expect(() =>
      assertInstallerPrerequisites("darwin", () => true),
    ).not.toThrow();
    expect(() =>
      assertInstallerPrerequisites("win32", () => false),
    ).not.toThrow();
  });
});
