import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  deriveSchemaVersion,
  prepareRelease,
  previousReleaseTag,
  releaseDocuments,
} from "./prepare-release";

test("CI helper writes publishable files from tagged source and signed bundle", async () => {
  const rootDir = await mkdtemp(join(tmpdir(), "openpiso-release-"));
  try {
    await mkdir(join(rootDir, "apps/admin/src-tauri/src/db"), {
      recursive: true,
    });
    await mkdir(join(rootDir, "docs/releases"), { recursive: true });
    await mkdir(join(rootDir, "bundle"));
    await Bun.write(
      join(rootDir, "apps/admin/src-tauri/src/db/mod.rs"),
      await Bun.file(
        new URL("../apps/admin/src-tauri/src/db/mod.rs", import.meta.url),
      ).text(),
    );
    await Bun.write(
      join(rootDir, "apps/admin/src-tauri/tauri.conf.json"),
      JSON.stringify({
        version: "0.12.0",
        plugins: { updater: { pubkey: "configured-public-key" } },
      }),
    );
    await Bun.write(
      join(rootDir, "docs/releases/v0.12.0.md"),
      "## New\n- Owner App Update.\n",
    );
    await Bun.write(
      join(rootDir, "previous.json"),
      JSON.stringify({ version: "0.11.9", schema_version: 58 }),
    );
    const installer = join(
      rootDir,
      "bundle/OpenPiso Central Terminal_0.12.0_x64-setup.exe",
    );
    await Bun.write(installer, "installer fixture");
    await Bun.write(`${installer}.sig`, "signature fixture\n");
    const options = {
      rootDir,
      tag: "v0.12.0",
      previousFile: join(rootDir, "previous.json"),
      bundleDir: join(rootDir, "bundle"),
      outputDir: join(rootDir, "publish"),
      pubDate: "2026-10-13T12:00:00Z",
    };
    const clientInstaller = join(
      rootDir,
      "bundle/OpenPiso Client_0.12.0_x64-setup.exe",
    );
    await expect(prepareRelease(options)).rejects.toThrow(
      "Client NSIS installer is missing",
    );
    await Bun.write(clientInstaller, "");
    await expect(prepareRelease(options)).rejects.toThrow(
      "Client NSIS installer is missing",
    );
    await Bun.write(clientInstaller, "client installer fixture");
    await prepareRelease(options);
    const latest = await Bun.file(join(rootDir, "publish/latest.json")).json();
    expect(Object.keys(latest.platforms)).toEqual(["windows-x86_64"]);
    expect(latest.platforms["windows-x86_64"].url).toContain(
      "OpenPiso%20Central%20Terminal_0.12.0_x64-setup.exe",
    );
    expect(
      await Bun.file(join(rootDir, "publish/release-body.md")).text(),
    ).toBe("## New\n- Owner App Update.");
    expect(
      await Bun.file(join(rootDir, "publish/release.json")).json(),
    ).toEqual({
      version: "0.12.0",
      schema_version: 58,
      notes: "## New\n- Owner App Update.",
      pub_date: "2026-10-13T12:00:00Z",
    });
    expect(
      (await Bun.file(join(rootDir, "publish/latest.json")).json()).platforms[
        "windows-x86_64"
      ].signature,
    ).toBe("signature fixture");
    await rm(`${installer}.sig`);
    await expect(prepareRelease(options)).rejects.toThrow();
  } finally {
    await rm(rootDir, { recursive: true, force: true });
  }
});

test("baseline is the newest older stable release, not a draft, preview or rerun", () => {
  const releases = [
    { tag_name: "v0.9.0", draft: false, prerelease: false },
    { tag_name: "v0.11.9", draft: false, prerelease: false },
    { tag_name: "v0.12.0", draft: false, prerelease: false },
    { tag_name: "v0.13.0", draft: true, prerelease: false },
    { tag_name: "v0.14.0-beta.1", draft: false, prerelease: true },
  ];
  expect(previousReleaseTag([releases], "v0.12.0")).toBe("v0.11.9");
  expect(previousReleaseTag([[]], "v0.12.0")).toBeNull();
  expect(() => previousReleaseTag([releases], "v0.11.0")).toThrow(
    "newer stable release",
  );
});

const input = {
  tag: "v0.12.0",
  version: "0.12.0",
  schemaVersion: 59,
  previousSchemaVersion: 58,
  notes: "## Fixed\n- Customers keep their paid time.\n",
  pubDate: "2026-10-13T12:00:00Z",
  signature: "signed-installer\n",
};

test("schema-raising release shares reviewed notes and warning across both manifests", () => {
  const result = releaseDocuments(input);
  const notes =
    "## Fixed\n- Customers keep their paid time.\n\nDatabase changes: Schema Version 59. You cannot roll back below this Schema Version without restoring a Pre-Migration Backup.";
  expect(result.body).toBe(notes);
  expect(result.release).toEqual({
    version: "0.12.0",
    schema_version: 59,
    notes,
    pub_date: "2026-10-13T12:00:00Z",
  });
  expect(result.latest).toEqual({
    version: "0.12.0",
    notes,
    pub_date: "2026-10-13T12:00:00Z",
    platforms: {
      "windows-x86_64": {
        signature: "signed-installer",
        url: "https://github.com/H3atman/openpiso-releases/releases/download/v0.12.0/OpenPiso%20Central%20Terminal_0.12.0_x64-setup.exe",
      },
    },
  });
});

test("same-schema release keeps notes untouched; first release warns conservatively", () => {
  expect(releaseDocuments({ ...input, previousSchemaVersion: 59 }).body).toBe(
    "## Fixed\n- Customers keep their paid time.",
  );
  expect(
    releaseDocuments({ ...input, previousSchemaVersion: null }).body,
  ).toContain("Database changes: Schema Version 59.");
});

test("publication refuses mismatched tags, preview versions, invalid metadata and schema regression", () => {
  for (const override of [
    { tag: "v0.12.1" },
    { tag: "v0.12.0-beta.1", version: "0.12.0-beta.1" },
    { notes: " " },
    { signature: " " },
    { pubDate: "not a date" },
    { previousSchemaVersion: 60 },
    { schemaVersion: 0 },
  ])
    expect(() => releaseDocuments({ ...input, ...override })).toThrow();
});

test("release schema comes from registered migrations, checked against the guard", () => {
  const source = `pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 2;
    let migrations: &[(i64, &str)] = &[
      (1_i64, include_str!("../../migrations/0001_initial.sql")),
      (2_i64, include_str!("../../migrations/0002_accounts.sql")),
    ];`;
  expect(deriveSchemaVersion(source)).toBe(2);
  expect(() => deriveSchemaVersion(source.replace("= 2;", "= 3;"))).toThrow();
  expect(() => deriveSchemaVersion(source.replace("2_i64", "3_i64"))).toThrow();
  expect(() => deriveSchemaVersion("no migration table")).toThrow();
});
