import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { parseArgs } from "node:util";

export const RELEASE_REPOSITORY = "H3atman/openpiso";

function stableVersion(version: string): number[] {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error(`Expected stable SemVer: ${version}`);
  }
  const parts = version.split(".").map(Number);
  if (parts.some((part) => !Number.isSafeInteger(part)))
    throw new Error("Invalid version number");
  return parts;
}

// gh api --paginate --slurp returns an array of release-list pages.
export function previousReleaseTag(pages: unknown, tag: string): string | null {
  const current = stableVersion(tag.replace(/^v/, ""));
  if (!Array.isArray(pages) || pages.some((page) => !Array.isArray(page)))
    throw new Error("Invalid GitHub releases response");
  let previous: { tag: string; parts: number[] } | null = null;
  const compare = (a: number[], b: number[]) =>
    a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
  for (const release of pages.flat() as unknown[]) {
    if (
      typeof release !== "object" ||
      release === null ||
      !("tag_name" in release) ||
      typeof release.tag_name !== "string" ||
      !("draft" in release) ||
      typeof release.draft !== "boolean" ||
      !("prerelease" in release) ||
      typeof release.prerelease !== "boolean"
    )
      throw new Error("Invalid GitHub release");
    if (release.draft || release.prerelease) continue;
    const parts = stableVersion(release.tag_name.replace(/^v/, ""));
    if (compare(parts, current) > 0)
      throw new Error("Cannot mark latest behind a newer stable release");
    if (
      compare(parts, current) < 0 &&
      (!previous || compare(parts, previous.parts) > 0)
    )
      previous = { tag: release.tag_name, parts };
  }
  return previous?.tag ?? null;
}

export function releaseDocuments(input: {
  repository?: string;
  tag: string;
  version: string;
  schemaVersion: number;
  previousSchemaVersion: number | null;
  notes: string;
  pubDate: string;
  signature: string;
}) {
  const repository = input.repository ?? RELEASE_REPOSITORY;
  if (!/^[A-Za-z0-9-]+\/[A-Za-z0-9_.-]+$/.test(repository))
    throw new Error("Expected release repository: owner/name");
  stableVersion(input.version);
  if (input.tag !== `v${input.version}`)
    throw new Error("Tag must match Admin version");
  if (
    !Number.isSafeInteger(input.schemaVersion) ||
    input.schemaVersion < 1 ||
    (input.previousSchemaVersion !== null &&
      (!Number.isSafeInteger(input.previousSchemaVersion) ||
        input.previousSchemaVersion < 1 ||
        input.previousSchemaVersion > input.schemaVersion))
  ) {
    throw new Error("Invalid or regressing Schema Version");
  }
  if (
    !input.notes.trim() ||
    !input.signature.trim() ||
    !/^\d{4}-\d{2}-\d{2}T/.test(input.pubDate) ||
    !Number.isFinite(Date.parse(input.pubDate))
  ) {
    throw new Error(
      "Release requires notes, signature and an ISO publication date",
    );
  }
  let notes = input.notes.trim();
  if (
    input.previousSchemaVersion === null ||
    input.schemaVersion > input.previousSchemaVersion
  ) {
    notes += `\n\nDatabase changes: Schema Version ${input.schemaVersion}. You cannot roll back below this Schema Version without restoring a Pre-Migration Backup.`;
  }
  const installer = `OpenPiso Central Terminal_${input.version}_x64-setup.exe`;
  const release = {
    version: input.version,
    schema_version: input.schemaVersion,
    notes,
    pub_date: input.pubDate,
  };
  return {
    installer,
    body: notes,
    release,
    latest: {
      version: input.version,
      notes,
      pub_date: input.pubDate,
      platforms: {
        "windows-x86_64": {
          signature: input.signature.trim(),
          // GitHub stores uploaded asset names with spaces replaced by dots.
          url: `https://github.com/${repository}/releases/download/${input.tag}/${installer.replaceAll(" ", ".")}`,
        },
      },
    },
  };
}

// Read the registered migration table, not unregistered files in migrations/.
export function deriveSchemaVersion(source: string): number {
  const table =
    /let migrations:\s*&\[\(i64, &str\)\]\s*=\s*&\[([\s\S]*?)\];/.exec(
      source,
    )?.[1];
  const guard = /const CURRENT_SCHEMA_VERSION:\s*i64\s*=\s*(\d+);/.exec(
    source,
  )?.[1];
  const entries = [
    ...(table ?? "").matchAll(
      /\(\s*(\d+)_i64,\s*include_str!\("\.\.\/\.\.\/migrations\/(\d+)_[^"]+\.sql"\)/g,
    ),
  ];
  if (
    !guard ||
    !entries.length ||
    entries.some(
      (entry, index) =>
        Number(entry[1]) !== index + 1 || Number(entry[2]) !== index + 1,
    )
  ) {
    throw new Error("Invalid or non-contiguous Admin migration table");
  }
  const version = Number(entries.at(-1)![1]);
  if (version !== Number(guard))
    throw new Error("Migration table disagrees with CURRENT_SCHEMA_VERSION");
  return version;
}

async function releaseSource(rootDir: string, tag: string) {
  const config: unknown = await Bun.file(
    join(rootDir, "apps/admin/src-tauri/tauri.conf.json"),
  ).json();
  if (
    typeof config !== "object" ||
    config === null ||
    !("version" in config) ||
    typeof config.version !== "string"
  )
    throw new Error("Missing Admin version");
  stableVersion(config.version);
  if (tag !== `v${config.version}`)
    throw new Error("Tag must match Admin version");
  if (
    !("plugins" in config) ||
    typeof config.plugins !== "object" ||
    config.plugins === null ||
    !("updater" in config.plugins) ||
    typeof config.plugins.updater !== "object" ||
    config.plugins.updater === null ||
    !("pubkey" in config.plugins.updater) ||
    typeof config.plugins.updater.pubkey !== "string" ||
    !config.plugins.updater.pubkey.trim() ||
    config.plugins.updater.pubkey.includes("TODO")
  ) {
    throw new Error("Configure the Admin updater public key before publishing");
  }
  const notes = await Bun.file(join(rootDir, `docs/releases/${tag}.md`)).text();
  if (!notes.trim()) throw new Error("Reviewed release notes are required");
  const schemaVersion = deriveSchemaVersion(
    await Bun.file(join(rootDir, "apps/admin/src-tauri/src/db/mod.rs")).text(),
  );
  return { version: config.version, notes, schemaVersion };
}

export async function prepareRelease(options: {
  repository?: string;
  rootDir: string;
  tag: string;
  previousFile: string | null;
  bundleDir: string;
  outputDir: string;
  pubDate: string;
}): Promise<void> {
  const source = await releaseSource(options.rootDir, options.tag);
  let previousSchemaVersion: number | null = null;
  if (options.previousFile) {
    const previous: unknown = await Bun.file(options.previousFile).json();
    if (
      typeof previous !== "object" ||
      previous === null ||
      !("schema_version" in previous) ||
      typeof previous.schema_version !== "number"
    )
      throw new Error("Previous release has no valid Schema Version");
    previousSchemaVersion = previous.schema_version;
  }
  const installer = join(
    options.bundleDir,
    `OpenPiso Central Terminal_${source.version}_x64-setup.exe`,
  );
  if (!(await Bun.file(installer).exists()) || !Bun.file(installer).size)
    throw new Error("Signed NSIS installer is missing");
  const clientInstaller = join(
    options.bundleDir,
    `OpenPiso Client_${source.version}_x64-setup.exe`,
  );
  if (
    !(await Bun.file(clientInstaller).exists()) ||
    !Bun.file(clientInstaller).size
  )
    throw new Error("Client NSIS installer is missing");
  const documents = releaseDocuments({
    ...source,
    repository: options.repository,
    tag: options.tag,
    previousSchemaVersion,
    pubDate: options.pubDate,
    signature: await Bun.file(`${installer}.sig`).text(),
  });
  await mkdir(options.outputDir, { recursive: true });
  await Bun.write(join(options.outputDir, "release-body.md"), documents.body);
  await Bun.write(
    join(options.outputDir, "release.json"),
    JSON.stringify(documents.release, null, 2) + "\n",
  );
  await Bun.write(
    join(options.outputDir, "latest.json"),
    JSON.stringify(documents.latest, null, 2) + "\n",
  );
}

if (import.meta.main) {
  const { values, positionals } = parseArgs({
    args: Bun.argv.slice(2),
    options: { repository: { type: "string" } },
    allowPositionals: true,
  });
  const [command, tag, ...args] = positionals;
  if (command === "validate" && tag) {
    await releaseSource(process.cwd(), tag);
  } else if (command === "previous" && tag && args[0]) {
    console.log(previousReleaseTag(await Bun.file(args[0]).json(), tag) ?? "");
  } else if (command === "prepare" && tag && args.length === 3) {
    await prepareRelease({
      rootDir: process.cwd(),
      repository: values.repository,
      tag,
      previousFile: args[0] === "-" ? null : args[0],
      bundleDir: args[1],
      outputDir: args[2],
      pubDate: new Date().toISOString(),
    });
  } else {
    throw new Error(
      "Usage: prepare-release.ts validate <tag> | previous <tag> <releases.json> | prepare <tag> <previous.json|-> <bundle-dir> <output-dir> [--repository owner/name]",
    );
  }
}
