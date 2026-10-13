import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { parseCli, release, type ReleaseOptions } from "./bump-version";
import { RELEASE_REPOSITORY } from "./prepare-release";

const manifests = [
  "package.json",
  "apps/admin/package.json",
  "apps/client/package.json",
  "apps/web-admin/package.json",
  "packages/ui/package.json",
  "apps/admin/src-tauri/tauri.conf.json",
  "apps/client/src-tauri/tauri.conf.json",
];
const download = `https://github.com/${RELEASE_REPOSITORY}/releases/download/v0.11.6`;
const installerUrl = `${download}/OpenPiso.Central.Terminal_0.11.6_x64-setup.exe`;
const assets = [
  "OpenPiso.Central.Terminal_0.11.6_x64-setup.exe",
  "OpenPiso.Central.Terminal_0.11.6_x64-setup.exe.sig",
  "OpenPiso.Client_0.11.6_x64-setup.exe",
  "latest.json",
  "release.json",
];
const runList = [
  "gh",
  "run",
  "list",
  "-R",
  RELEASE_REPOSITORY,
  "--workflow",
  "release.yml",
  "--event",
  "push",
  "--json",
  "databaseId,headBranch",
];
const roots: string[] = [];
afterEach(async () => {
  await Promise.all(
    roots.splice(0).map((root) => rm(root, { recursive: true, force: true })),
  );
});

async function fixture(overrides: Partial<ReleaseOptions> = {}) {
  const rootDir = await mkdtemp(join(tmpdir(), "openpiso-release-"));
  roots.push(rootDir);
  for (const path of manifests) {
    await mkdir(join(rootDir, path, ".."), { recursive: true });
    await Bun.write(join(rootDir, path), JSON.stringify({ version: "0.11.5" }));
  }
  await Bun.write(
    join(rootDir, "apps/admin/src-tauri/tauri.conf.json"),
    JSON.stringify({
      version: "0.11.5",
      plugins: { updater: { pubkey: "dW50cnVzdGVkIGNvbW1lbnQ6" } },
    }),
  );
  await Bun.write(
    join(rootDir, "Cargo.toml"),
    '[workspace.package]\nversion = "0.11.5"\n',
  );
  const commands: string[][] = [];
  const options: ReleaseOptions = {
    rootDir,
    kind: "patch",
    editor: ["fake-editor"],
    sleep: async () => {},
    run: async (command) => {
      commands.push(command);
      const [tool, sub, action] = command;
      if (sub === "branch") return { exitCode: 0, stdout: "main\n" };
      if (command.includes("--max-parents=0"))
        return { exitCode: 0, stdout: "rootsha\nothersha\n" };
      if (sub === "rev-list") return { exitCode: 0, stdout: "0\t0\n" };
      if (tool === "gh" && sub === "secret")
        return {
          exitCode: 0,
          stdout: JSON.stringify([
            { name: "TAURI_SIGNING_PRIVATE_KEY" },
            { name: "TAURI_SIGNING_PRIVATE_KEY_PASSWORD" },
            { name: "OPENPISO_POSTHOG_TOKEN" },
          ]),
        };
      if (tool === "gh" && sub === "run" && action === "list")
        return {
          exitCode: 0,
          stdout: JSON.stringify([
            { databaseId: 7, headBranch: "main" },
            { databaseId: 42, headBranch: "v0.11.6" },
          ]),
        };
      if (tool === "gh" && sub === "release")
        return {
          exitCode: 0,
          stdout: JSON.stringify({
            isDraft: false,
            isPrerelease: false,
            assets: assets.map((name) => ({ name, size: 1 })),
          }),
        };
      if (tool === "gh" && sub === "api")
        return { exitCode: 0, stdout: "v0.11.6\n" };
      if (tool === "curl" && sub === "-fsSL")
        return {
          exitCode: 0,
          stdout: JSON.stringify({
            platforms: { "windows-x86_64": { url: installerUrl } },
          }),
        };
      if (command[1] === "describe" || command[0] === "codex")
        return { exitCode: 1, stdout: "" };
      if (command[1] === "log")
        return {
          exitCode: 0,
          stdout:
            "feat: owner update panel\nfix(timer): preserve remaining time\nrefactor: simplify setup\nchore: dependencies\n",
        };
      return { exitCode: 0, stdout: "" };
    },
    confirm: async () => false,
    ...overrides,
  };
  return { options, commands, rootDir };
}

describe("release workflow", () => {
  test("does not commit or tag when the reviewed notes are empty", async () => {
    const { options, commands } = await fixture({ yes: true });
    const run = options.run;
    options.run = async (command) => {
      if (command[0] === "fake-editor") await Bun.write(command[1], "  \n");
      return run(command);
    };
    await expect(release(options)).rejects.toThrow(
      "Release notes must not be empty",
    );
    expect(
      commands.some(
        (command) =>
          ["commit", "push"].includes(command[1]) ||
          (command[1] === "tag" && command.length === 3),
      ),
    ).toBe(false);
  });

  test("falls back from a malformed AI draft with at most eight supported bullets", async () => {
    const { options, rootDir } = await fixture();
    const run = options.run;
    options.run = async (command) => {
      if (command[0] === "codex")
        return { exitCode: 0, stdout: "Unstructured invented prose" };
      if (command[1] === "log")
        return {
          exitCode: 0,
          stdout: [
            "Merge branch unrelated",
            "feat: update panel",
            "fix: timers",
            "feat: rollback",
            "fix: installer",
            "perf: faster startup",
            "docs: setup guide",
            "chore: dependencies",
            "refactor: clearer settings",
            "feat: ninth change",
          ].join("\n"),
        };
      return run(command);
    };
    await release(options);
    const notes = await Bun.file(
      join(rootDir, "docs/releases/v0.11.6.md"),
    ).text();
    expect(notes).toContain("## New\n- update panel\n- rollback");
    expect(notes).toContain("## Fixed\n- timers\n- installer");
    expect(notes).toContain("## Changed\n- faster startup");
    expect(notes.match(/^- /gm)).toHaveLength(8);
    expect(notes).not.toContain("invented");
    expect(notes).not.toContain("ninth change");
    expect(notes).not.toContain("Merge branch");
  });

  test("uses the latest reachable stable tag and retains a valid AI draft", async () => {
    const { options, commands, rootDir } = await fixture();
    const run = options.run;
    options.run = async (command) => {
      if (command[1] === "tag" && command.includes("--merged")) {
        commands.push(command);
        return { exitCode: 0, stdout: "v0.12.0-beta.1\nv0.11.5\nv0.11.4\n" };
      }
      if (command[0] === "codex") {
        commands.push(command);
        return {
          exitCode: 0,
          stdout:
            "## New\n- Review updates from your phone.\n\n## Fixed\n\n## Changed",
        };
      }
      return run(command);
    };
    await release(options);
    expect(commands).toContainEqual([
      "git",
      "log",
      "v0.11.5..HEAD",
      "--format=%s",
      "--reverse",
    ]);
    expect(
      await Bun.file(join(rootDir, "docs/releases/v0.11.6.md")).text(),
    ).toContain("- Review updates from your phone.");
  });

  test.each([
    ["patch", "0.11.6"],
    ["minor", "0.12.0"],
    ["major", "1.0.0"],
  ] as const)(
    "dry-run previews %s without writing or running release tools",
    async (kind, version) => {
      const { options, commands, rootDir } = await fixture({
        kind,
        dryRun: true,
      });
      await release(options);
      expect(commands).toContainEqual(["git", "tag", "--list", `v${version}`]);
      expect(commands.every(([tool]) => tool === "git" || tool === "gh")).toBe(
        true,
      );
      expect(commands).toContainEqual([
        "gh",
        "secret",
        "list",
        "-R",
        RELEASE_REPOSITORY,
        "--json",
        "name",
      ]);
      expect(
        (await Bun.file(join(rootDir, "package.json")).json()).version,
      ).toBe("0.11.5");
      expect(
        await Bun.file(join(rootDir, `docs/releases/v${version}.md`)).exists(),
      ).toBe(false);
    },
  );

  test.each([false, true])(
    "reviews notes before committing/tagging and gates push (yes=%s)",
    async (yes) => {
      const { options, rootDir, commands } = await fixture({ yes });
      let prompted = false;
      options.confirm = async () => {
        prompted = true;
        return false;
      };
      const run = options.run;
      options.run = async (command) => {
        if (command[0] === "fake-editor") {
          await Bun.write(command[1], "# Reviewed notes\n");
        }
        if (command[1] === "commit") {
          expect(
            await Bun.file(join(rootDir, "docs/releases/v0.11.6.md")).text(),
          ).toBe("# Reviewed notes\n");
        }
        return run(command);
      };
      await release(options);
      expect(commands).toContainEqual([
        "fake-editor",
        join(rootDir, "docs/releases/v0.11.6.md"),
      ]);
      const commit = commands.findIndex((command) => command[1] === "commit");
      const tag = commands.findIndex(
        (command) => command.length === 3 && command[1] === "tag",
      );
      expect(commit).toBeGreaterThan(
        commands.findIndex(([tool]) => tool === "fake-editor"),
      );
      expect(tag).toBeGreaterThan(commit);
      expect(commands[tag]).toEqual(["git", "tag", "v0.11.6"]);
      expect(prompted).toBe(!yes);
      // Declining the push never watches the workflow.
      expect(
        commands.some(([tool, sub]) => tool === "gh" && sub === "run"),
      ).toBe(yes);
      expect(commands.filter((command) => command[1] === "push")).toEqual(
        yes
          ? [["git", "push", "--atomic", "origin", "main", "refs/tags/v0.11.6"]]
          : [],
      );
    },
  );
  test("bumps lockstep and writes grouped fallback notes for the first release", async () => {
    const { options, rootDir, commands } = await fixture();
    await release(options);
    for (const path of manifests) {
      expect((await Bun.file(join(rootDir, path)).json()).version).toBe(
        "0.11.6",
      );
    }
    expect(await Bun.file(join(rootDir, "Cargo.toml")).text()).toContain(
      'version = "0.11.6"',
    );
    expect(
      await Bun.file(join(rootDir, "docs/releases/v0.11.6.md")).text(),
    ).toBe(
      "# v0.11.6\n\n## New\n- owner update panel\n\n## Fixed\n- preserve remaining time\n\n## Changed\n- simplify setup\n- dependencies\n",
    );
    expect(commands).toContainEqual([
      "git",
      "log",
      "rootsha..HEAD",
      "--format=%s",
      "--reverse",
    ]);
    expect(commands).toContainEqual([
      "git",
      "rev-list",
      "--max-parents=0",
      "HEAD",
    ]);
    expect(commands).toContainEqual(["bun", "install", "--lockfile-only"]);
    expect(commands).toContainEqual([
      "cargo",
      "update",
      "--workspace",
      "--offline",
    ]);
  });
});

describe("release CLI", () => {
  test.each([
    { args: [] },
    { args: ["banana"] },
    { args: ["patch", "--bogus"] },
  ])(
    "rejects invalid arguments without starting a release",
    async ({ args }) => {
      const child = Bun.spawn(
        ["bun", join(import.meta.dir, "bump-version.ts"), ...args],
        {
          stdout: "pipe",
          stderr: "pipe",
        },
      );
      expect(await child.exited).toBe(1);
      expect(await new Response(child.stderr).text()).toContain("Usage:");
    },
  );

  test("accepts --no-watch alongside --yes and --dry-run", () => {
    expect(parseCli(["minor", "--no-watch", "--yes", "--dry-run"])).toEqual({
      kind: "minor",
      yes: true,
      dryRun: true,
      noWatch: true,
    });
    expect(parseCli(["patch"])).toEqual({
      kind: "patch",
      yes: false,
      dryRun: false,
      noWatch: false,
    });
    expect(() => parseCli(["patch", "--watch"])).toThrow("--no-watch");
  });
});

describe("release preconditions", () => {
  test.each([
    ["equal", "0\t0\n", false, false],
    ["behind only", "0\t3\n", true, false],
    ["ahead only", "1\t0\n", false, true],
    ["diverged", "1\t3\n", false, true],
  ] as const)(
    "syncs main before release work (%s)",
    async (_, counts, fastForward, abort) => {
      const { options, commands, rootDir } = await fixture({ dryRun: true });
      const run = options.run;
      options.run = async (command) => {
        if (command[1] === "rev-list") {
          commands.push(command);
          return { exitCode: 0, stdout: counts };
        }
        return run(command);
      };
      if (abort) {
        await expect(release(options)).rejects.toThrow(
          "Push or rebase onto origin/main first",
        );
      } else {
        await release(options);
      }
      expect(commands).toEqual([
        ["git", "status", "--porcelain"],
        ["git", "branch", "--show-current"],
        ["git", "fetch", "origin", "main"],
        ["git", "rev-list", "--left-right", "--count", "main...origin/main"],
        ...(fastForward ? [["git", "merge", "--ff-only", "origin/main"]] : []),
        ...(abort
          ? []
          : [
              ["git", "tag", "--list", "v0.11.6"],
              [
                "gh",
                "secret",
                "list",
                "-R",
                RELEASE_REPOSITORY,
                "--json",
                "name",
              ],
            ]),
      ]);
      expect(
        (await Bun.file(join(rootDir, "package.json")).json()).version,
      ).toBe("0.11.5");
    },
  );

  test("stops on fetch failure before modifying versions", async () => {
    const { options, commands, rootDir } = await fixture();
    const run = options.run;
    options.run = async (command) => {
      if (command[1] === "fetch") {
        commands.push(command);
        return { exitCode: 1, stdout: "" };
      }
      return run(command);
    };
    await expect(release(options)).rejects.toThrow(
      "Command failed (1): git fetch origin main",
    );
    expect(commands).toEqual([
      ["git", "status", "--porcelain"],
      ["git", "branch", "--show-current"],
      ["git", "fetch", "origin", "main"],
    ]);
    expect((await Bun.file(join(rootDir, "package.json")).json()).version).toBe(
      "0.11.5",
    );
  });

  test("refuses an existing target tag without modifying versions", async () => {
    const { options, rootDir, commands } = await fixture();
    const run = options.run;
    options.run = async (command) =>
      command[1] === "tag"
        ? { exitCode: 0, stdout: "v0.11.6\n" }
        : run(command);
    await expect(release(options)).rejects.toThrow("v0.11.6 already exists");
    expect((await Bun.file(join(rootDir, "package.json")).json()).version).toBe(
      "0.11.5",
    );
    expect(commands.some(([tool]) => tool !== "git")).toBe(false);
  });
  test.each([
    ["dirty tree", "status", " M package.json\n", "clean"],
    ["non-main branch", "branch", "feature\n", "main"],
  ])(
    "refuses %s before writing or invoking release tools",
    async (_, gitCommand, output, message) => {
      const { options, commands, rootDir } = await fixture();
      const run = options.run;
      options.run = async (command) =>
        command[1] === gitCommand
          ? { exitCode: 0, stdout: output }
          : run(command);
      await expect(release(options)).rejects.toThrow(message);
      expect(
        (await Bun.file(join(rootDir, "package.json")).json()).version,
      ).toBe("0.11.5");
      expect(commands.some(([tool]) => tool !== "git")).toBe(false);
    },
  );
});

describe("release prechecks", () => {
  test.each(
    (["a missing secret", "a placeholder public key"] as const).flatMap(
      (problem) => [false, true].map((dryRun) => ({ problem, dryRun })),
    ),
  )(
    "refuses $problem before modifying versions (dryRun: $dryRun)",
    async ({ problem, dryRun }) => {
      const { options, commands, rootDir } = await fixture({ dryRun });
      const run = options.run;
      if (problem === "a missing secret") {
        options.run = async (command) => {
          if (command[1] === "secret") {
            commands.push(command);
            return {
              exitCode: 0,
              stdout: JSON.stringify([{ name: "TAURI_SIGNING_PRIVATE_KEY" }]),
            };
          }
          return run(command);
        };
      } else {
        await Bun.write(
          join(rootDir, "apps/admin/src-tauri/tauri.conf.json"),
          JSON.stringify({
            version: "0.11.5",
            plugins: {
              updater: {
                pubkey:
                  "TODO_REPLACE_WITH_MINISIGN_PUBLIC_KEY_SEE_DOCS_RELEASING",
              },
            },
          }),
        );
      }
      await expect(release(options)).rejects.toThrow(
        problem === "a missing secret"
          ? "TAURI_SIGNING_PRIVATE_KEY_PASSWORD, OPENPISO_POSTHOG_TOKEN"
          : "updater public key",
      );
      for (const path of [...manifests, "Cargo.toml"]) {
        expect(await Bun.file(join(rootDir, path)).text()).toContain("0.11.5");
        expect(await Bun.file(join(rootDir, path)).text()).not.toContain(
          "0.11.6",
        );
      }
      expect(commands.some(([tool]) => tool !== "git" && tool !== "gh")).toBe(
        false,
      );
    },
  );
});

describe("release watch", () => {
  async function pushed(
    respond: (command: string[]) => { exitCode: number; stdout: string } | null,
    overrides: Partial<ReleaseOptions> = {},
  ) {
    const fake = await fixture({ yes: true, ...overrides });
    const run = fake.options.run;
    fake.options.run = async (command) => {
      const response = respond(command);
      if (!response) return run(command);
      fake.commands.push(command);
      return response;
    };
    return fake;
  }

  test("watches the tag's workflow run and verifies the public release", async () => {
    const { options, commands } = await pushed(() => null);
    await release(options);
    const after = commands.slice(
      commands.findIndex((command) => command[1] === "push") + 1,
    );
    expect(after).toEqual([
      runList,
      ["gh", "run", "watch", "42", "-R", RELEASE_REPOSITORY, "--exit-status"],
      [
        "gh",
        "release",
        "view",
        "v0.11.6",
        "-R",
        RELEASE_REPOSITORY,
        "--json",
        "isDraft,isPrerelease,assets",
      ],
      [
        "gh",
        "api",
        `repos/${RELEASE_REPOSITORY}/releases/latest`,
        "--jq",
        ".tag_name",
      ],
      ["curl", "-fsSL", `${download}/latest.json`],
      ["curl", "-fsIL", installerUrl],
    ]);
  });

  test("--no-watch pushes without watching", async () => {
    const { options, commands } = await pushed(() => null, { noWatch: true });
    await release(options);
    expect(commands.at(-1)?.[1]).toBe("push");
  });

  test("polls until the tag's run appears, ignoring other branches", async () => {
    let polls = 0;
    let sleeps = 0;
    const { options, commands } = await pushed(
      (command) => {
        if (command[2] !== "list" || command[1] !== "run") return null;
        polls += 1;
        return {
          exitCode: 0,
          stdout: JSON.stringify(
            polls < 3
              ? [{ databaseId: 7, headBranch: "main" }]
              : [{ databaseId: 9, headBranch: "v0.11.6" }],
          ),
        };
      },
      {
        sleep: async () => {
          sleeps += 1;
        },
      },
    );
    await release(options);
    expect(polls).toBe(3);
    expect(sleeps).toBe(2);
    expect(commands).toContainEqual([
      "gh",
      "run",
      "watch",
      "9",
      "-R",
      RELEASE_REPOSITORY,
      "--exit-status",
    ]);
  });

  test("gives up when the tag's run never appears", async () => {
    let polls = 0;
    const { options, commands } = await pushed((command) => {
      if (command[2] !== "list" || command[1] !== "run") return null;
      polls += 1;
      return {
        exitCode: 0,
        stdout: JSON.stringify([{ databaseId: 7, headBranch: "main" }]),
      };
    });
    await expect(release(options)).rejects.toThrow(
      "No release.yml run found for v0.11.6",
    );
    expect(polls).toBeGreaterThan(1);
    expect(polls).toBeLessThanOrEqual(60);
    expect(commands.some((command) => command[2] === "watch")).toBe(false);
  });

  test("reports a failed workflow run", async () => {
    const { options, commands } = await pushed((command) =>
      command[2] === "watch" ? { exitCode: 1, stdout: "" } : null,
    );
    await expect(release(options)).rejects.toThrow(
      `Release workflow failed: https://github.com/${RELEASE_REPOSITORY}/actions/runs/42`,
    );
    expect(commands.some((command) => command[1] === "release")).toBe(false);
  });

  const view = (patch: Record<string, unknown>) => (command: string[]) =>
    command[1] === "release"
      ? {
          exitCode: 0,
          stdout: JSON.stringify({
            isDraft: false,
            isPrerelease: false,
            assets: assets.map((name) => ({ name })),
            ...patch,
          }),
        }
      : null;
  test.each([
    ["draft", view({ isDraft: true }), "v0.11.6 is still a draft"],
    ["prerelease", view({ isPrerelease: true }), "v0.11.6 is a prerelease"],
    [
      "not latest",
      (command: string[]) =>
        command[1] === "api" ? { exitCode: 0, stdout: "v0.11.5\n" } : null,
      "Latest release is v0.11.5, expected v0.11.6",
    ],
    [
      "missing asset",
      view({ assets: assets.slice(1).map((name) => ({ name })) }),
      "missing: OpenPiso.Central.Terminal_0.11.6_x64-setup.exe",
    ],
    [
      "extra asset",
      view({
        assets: [...assets, "OpenPiso Client_0.11.6_x64-setup.exe"].map(
          (name) => ({ name }),
        ),
      }),
      "unexpected: OpenPiso Client_0.11.6_x64-setup.exe",
    ],
    [
      "wrong feed URL",
      (command: string[]) =>
        command[1] === "-fsSL"
          ? {
              exitCode: 0,
              stdout: JSON.stringify({
                platforms: {
                  "windows-x86_64": {
                    url: `${download}/OpenPiso Central Terminal_0.11.6_x64-setup.exe`,
                  },
                },
              }),
            }
          : null,
      `latest.json points at ${download}/OpenPiso Central Terminal_0.11.6_x64-setup.exe, expected ${installerUrl}`,
    ],
    [
      "installer download failure",
      (command: string[]) =>
        command[1] === "-fsIL" ? { exitCode: 22, stdout: "" } : null,
      `Installer download failed: ${installerUrl}`,
    ],
  ] as const)(
    "refuses a published release with %s",
    async (_, respond, message) => {
      const { options } = await pushed(respond);
      await expect(release(options)).rejects.toThrow(message);
    },
  );
});
