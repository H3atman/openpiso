import { afterAll, describe, expect, test } from "bun:test";
import { createServer } from "node:net";

import { findAvailablePort, tauriDevCommand } from "./dev";

const occupiedPort = 14_221;
const server = createServer();

describe("findAvailablePort", () => {
  afterAll(() => server.close());

  test("skips a port occupied only on IPv6 loopback", async () => {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(occupiedPort, "::1", resolve);
    });

    expect(await findAvailablePort(occupiedPort)).toBeGreaterThan(occupiedPort);
  });
});

describe("tauriDevCommand", () => {
  test("uses the native Bun executable so JSON never passes through a cmd shim", () => {
    expect(tauriDevCommand("http://127.0.0.1:1421")).toEqual([
      process.execPath,
      "x",
      "tauri",
      "dev",
      "--config",
      '{"build":{"devUrl":"http://127.0.0.1:1421"},"bundle":{"resources":[]}}',
    ]);
  });
});
