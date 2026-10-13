import { createServer } from "node:net";

const DEFAULT_PORT = 1421;
const DEV_HOST = "127.0.0.1";
const LOOPBACK_HOSTS = [DEV_HOST, "::1"] as const;

async function isAvailableOnHost(port: number, host: string): Promise<boolean> {
  return new Promise<boolean>((resolve, reject) => {
    const server = createServer();

    server.once("error", (error: NodeJS.ErrnoException) => {
      if (error.code === "EADDRINUSE" || error.code === "EACCES") {
        resolve(false);
        return;
      }

      reject(error);
    });
    server.listen({ port, host, exclusive: true }, () => {
      server.close((error) => {
        if (error) reject(error);
        else resolve(true);
      });
    });
  });
}

export async function findAvailablePort(startPort: number): Promise<number> {
  for (let port = startPort; port <= 65_535; port += 1) {
    const availability = await Promise.all(
      LOOPBACK_HOSTS.map((host) => isAvailableOnHost(port, host)),
    );

    if (availability.every(Boolean)) return port;
  }

  throw new Error(
    `No available development port found from ${startPort} onward`,
  );
}

export function tauriDevCommand(devUrl: string): string[] {
  return [
    process.execPath,
    "x",
    "tauri",
    "dev",
    "--config",
    // Dev never bundles; drop resources so tauri-build does not require the
    // Windows-only Client Guard (`build:guard`) to exist.
    JSON.stringify({ build: { devUrl }, bundle: { resources: [] } }),
  ];
}

if (import.meta.main) {
  const port = await findAvailablePort(DEFAULT_PORT);
  const devUrl = `http://${DEV_HOST}:${port}`;

  if (port !== DEFAULT_PORT) {
    console.info(
      `Client port ${DEFAULT_PORT} is in use; using ${port} instead.`,
    );
  }

  const tauri = Bun.spawn(tauriDevCommand(devUrl), {
    cwd: import.meta.dir + "/..",
    env: {
      ...process.env,
      OPENPISO_CLIENT_DEV_PORT: String(port),
      // Seed the Fullscreen Lock's shutdown grace countdown so it is the
      // default view when running the client in dev for UI testing. Honoured
      // only by debug builds (see seed_dev_shutdown_countdown).
      OPENPISO_CLIENT_DEV_SHUTDOWN: "1",
    },
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });

  process.exitCode = await tauri.exited;
}
