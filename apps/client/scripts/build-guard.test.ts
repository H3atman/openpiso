import { describe, expect, test } from "bun:test";

import {
  guardBuildPlan,
  STAGED_GUARD_PATH,
  WINDOWS_TARGET,
} from "./build-guard";

const rootDir = `${import.meta.dir}/../../..`;

describe("guardBuildPlan", () => {
  test("keeps native Windows builds on Cargo's standard release path", () => {
    expect(guardBuildPlan("win32", WINDOWS_TARGET)).toEqual({
      command: ["cargo", "build", "-p", "client-guard", "--release"],
      sourcePath: STAGED_GUARD_PATH,
      stagedPath: STAGED_GUARD_PATH,
    });
  });

  test.each(["linux", "darwin"] as const)(
    "cross-builds a Windows executable from %s",
    (platform) => {
      expect(guardBuildPlan(platform, WINDOWS_TARGET)).toEqual({
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
      });
    },
  );

  test("rejects a native non-Windows Tauri build", () => {
    expect(() => guardBuildPlan("linux", "x86_64-unknown-linux-gnu")).toThrow(
      `Client Guard requires the ${WINDOWS_TARGET} target`,
    );
  });
});

describe("Client Guard installer path contract", () => {
  test("keeps the Tauri resource destination aligned with the NSIS service path", async () => {
    const config = (await Bun.file(
      `${rootDir}/apps/client/src-tauri/tauri.conf.json`,
    ).json()) as {
      bundle: { resources: Record<string, string> };
    };
    const hooks = await Bun.file(
      `${rootDir}/apps/client/src-tauri/windows/installer-hooks.nsh`,
    ).text();

    expect(
      config.bundle.resources["../../../target/release/client-guard.exe"],
    ).toBe("resources/client-guard.exe");
    expect(hooks).toContain(
      '!define GUARD_EXE "$INSTDIR\\resources\\client-guard.exe"',
    );
  });
});
