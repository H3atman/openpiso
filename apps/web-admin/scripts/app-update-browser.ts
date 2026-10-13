// ADR-0086: isolated Linux Admin + local Release Feed, then Owner Session E2E.
// Build assets + a debug Admin first; see docs/development.md. No real installer.
import { RELEASE_REPOSITORY } from "../../../scripts/prepare-release";
import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, openSync, closeSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createConnection } from "node:net";
import type { UpdateFixture } from "./owner-session-app-update";
import { stopProcessTree } from "../../client/scripts/browser-harness-process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const outDir = resolve(
  process.env.OPENPISO_E2E_OUT ??
    join(tmpdir(), `openpiso-app-update-${Date.now()}`),
);
mkdirSync(outDir, { recursive: true });
if (process.platform !== "linux")
  throw new Error(
    "This isolated launcher uses Linux XDG_DATA_HOME; use Linux with a display (or xvfb-run).",
  );
const binary = resolve(
  process.env.OPENPISO_ADMIN_BINARY ??
    join(
      process.env.CARGO_TARGET_DIR ?? join(root, "target"),
      "debug/openpiso-admin",
    ),
);
const version = (
  (await Bun.file(join(root, "package.json")).json()) as { version: string }
).version;
const [major, minor, patch] = version.split(".").map(Number);
if (patch < 2)
  throw new Error(
    "Fixture needs two older patch versions; update its version selection for this release.",
  );

async function requireFreePort(port: number): Promise<void> {
  const occupied = await new Promise<boolean>((done) => {
    const socket = createConnection({ host: "127.0.0.1", port });
    socket.setTimeout(1000);
    socket.once("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.once("error", () => done(false));
    socket.once("timeout", () => {
      socket.destroy();
      done(true);
    });
  });
  if (occupied)
    throw new Error(
      `Port ${port} occupied; stop the other Admin before running. No existing shop was touched.`,
    );
}
await requireFreePort(8080);
await requireFreePort(3210);

let fixture: UpdateFixture | undefined;
const feedRequests: { path: string; status: number }[] = [];
const feed = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    const url = new URL(request.url);
    let response: Response;
    if (url.pathname === `/repos/${RELEASE_REPOSITORY}/releases`) {
      response = Response.json(
        fixture
          ? [fixture.newer, fixture.compatible, fixture.incompatible].map(
              (v) => ({
                tag_name: `v${v}`,
                draft: false,
                prerelease: false,
                assets: ["release.json", "latest.json"].map((name) => ({
                  name,
                  browser_download_url: `${url.origin}/${v}/${name}`,
                })),
              }),
            )
          : [],
      );
    } else if (fixture && url.pathname.endsWith("/release.json")) {
      const v = url.pathname.split("/")[1];
      const schema =
        v === fixture.newer
          ? fixture.schema + 1
          : v === fixture.compatible
            ? fixture.schema
            : fixture.schema - 1;
      response = Response.json({
        version: v,
        schema_version: schema,
        pub_date: "2026-10-13T00:00:00Z",
        notes:
          v === fixture.newer
            ? "Fixture update summary\n\n**Full fixture notes**\n\n- Database upgrade for this fixture.\n\n<script>window.fixtureUnsafe = true</script>"
            : `Fixture notes for v${v}`,
      });
    } else if (url.pathname.endsWith("/latest.json")) {
      // Defense in depth: deliberately invalid manifest, no URL/signature/binary.
      // The browser scenario also never presses either final installer action.
      response = Response.json({ fixture: "installation forbidden" });
    } else response = new Response("Not found", { status: 404 });
    feedRequests.push({ path: url.pathname, status: response.status });
    return response;
  },
});

const dataHome = mkdtempSync(join(tmpdir(), "openpiso-app-update-data-"));
const log = openSync(join(outDir, "admin.log"), "a");
let admin: ReturnType<typeof Bun.spawn> | undefined;
const start = () =>
  Bun.spawn([binary], {
    cwd: root,
    env: {
      ...process.env,
      XDG_DATA_HOME: dataHome,
      OPENPISO_RELEASE_FEED_BASE_URL: feed.url.origin,
    },
    stdout: log,
    stderr: log,
  });
const stop = async () => {
  if (!admin) return;
  stopProcessTree(admin);
  await admin.exited;
  admin = undefined;
};
const ready = async () => {
  for (let i = 0; i < 300; i++) {
    if (admin?.exitCode !== null)
      throw new Error(
        `Admin exited before readiness; see ${join(outDir, "admin.log")}`,
      );
    try {
      if ((await fetch("http://127.0.0.1:8080/web-admin/session")).ok) return;
    } catch {}
    await Bun.sleep(100);
  }
  throw new Error("Admin readiness timed out");
};
let failure: unknown;
try {
  // Let the real Admin migrate its fresh DB, then seed only the restart input
  // while it is stopped. All observations go through the Owner UI/API.
  admin = start();
  await ready();
  await stop();
  const db = new Database(join(dataHome, "org.openpiso.admin/openpiso.db"));
  try {
    const applied = db
      .query<
        { schema: number },
        []
      >("SELECT MAX(version) AS schema FROM _openpiso_migrations")
      .get();
    if (!applied || applied.schema < 1)
      throw new Error("No applied Schema Version");
    fixture = {
      current: version,
      newer: `${major}.${minor + 1}.0`,
      compatible: `${major}.${minor}.${patch - 1}`,
      incompatible: `${major}.${minor}.${patch - 2}`,
      schema: applied.schema,
    };
    db.query(
      "INSERT INTO settings (key, value, updated_at) VALUES ('app_update_restart_marker', ?, CURRENT_TIMESTAMP)",
    ).run(
      JSON.stringify({
        fromVersion: fixture.compatible,
        targetVersion: fixture.current,
        startedAt: "2026-10-13T00:00:00Z",
      }),
    );
  } finally {
    db.close();
  }
  admin = start();
  await ready();
  const browser = Bun.spawn(
    [
      "bun",
      "run",
      join(root, "apps/web-admin/scripts/owner-session-browser.ts"),
    ],
    {
      cwd: root,
      env: {
        ...process.env,
        OPENPISO_OWNER_URL: "http://127.0.0.1:8080/admin",
        OPENPISO_OWNER_PIN: undefined,
        OPENPISO_E2E_OUT: outDir,
        OPENPISO_E2E_APP_UPDATE: JSON.stringify(fixture),
      },
      stdout: "inherit",
      stderr: "inherit",
    },
  );
  if ((await browser.exited) !== 0)
    throw new Error("Owner Session browser verification failed");
  if (feedRequests.some((r) => r.path.endsWith("/latest.json")))
    throw new Error("Installer manifest unexpectedly requested");
  console.log(`PASS: App Update isolated harness. Artifacts: ${outDir}`);
} catch (error) {
  failure = error;
} finally {
  await stop();
  feed.stop(true);
  closeSync(log);
  await Bun.write(
    join(outDir, "app-update-fixture.json"),
    JSON.stringify(
      {
        ok: !failure,
        fixture,
        feedRequests,
        dataHome,
        installerReached: feedRequests.some((r) =>
          r.path.endsWith("/latest.json"),
        ),
      },
      null,
      2,
    ),
  );
}
if (failure) {
  console.error(failure instanceof Error ? failure.message : String(failure));
  process.exit(1);
}
