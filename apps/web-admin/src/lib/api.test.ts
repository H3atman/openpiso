// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Api = typeof import("./api");

const fetchMock = vi.fn<typeof fetch>();

/** Answer every following request with a fresh response (a body reads once). */
function reply(body: unknown, status = 200): void {
  fetchMock.mockImplementation(async () =>
    new Response(typeof body === "string" ? body : JSON.stringify(body), { status }),
  );
}

/** The CSRF header the last request carried, or null when it sent none. */
function csrfOf(call: number): string | null {
  const init = fetchMock.mock.calls[call][1];
  return new Headers(init?.headers).get("X-CSRF-Token");
}

// The CSRF token lives in module memory, so every test starts a fresh page.
async function freshApi(): Promise<Api> {
  vi.resetModules();
  return import("./api");
}

describe("Owner Session CSRF handling", () => {
  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("sends the login CSRF token on mutations but not on reads", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-1" });
    await api.ownerLogin("1234");
    reply({ ok: true });
    await api.reportHumanActivity();
    reply({});
    await api.getTheme();

    expect(csrfOf(0)).toBeNull();
    expect(csrfOf(1)).toBe("csrf-1");
    expect(csrfOf(2)).toBeNull();
    expect(fetchMock.mock.calls[1][1]?.credentials).toBe("include");
  });

  it("does not hand the CSRF token to the caller of setup or login", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-secret" });

    expect(await api.setupOwner("1234")).toBeUndefined();
    expect(await api.ownerLogin("1234")).toBeUndefined();
  });

  it("restores the CSRF token after a reload through session bootstrap", async () => {
    const api = await freshApi();
    reply({ configured: true, authenticated: true, csrf_token: "csrf-reloaded" });
    const session = await api.getSession();
    reply({ ok: true });
    await api.reportHumanActivity();

    expect(session).toEqual({ configured: true, authenticated: true });
    expect(csrfOf(1)).toBe("csrf-reloaded");
  });

  it("forgets the CSRF token when bootstrap finds no session", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-1" });
    await api.ownerLogin("1234");
    reply({ configured: true, authenticated: false });
    await api.getSession();
    reply({ ok: true });
    await api.reportHumanActivity();

    expect(csrfOf(2)).toBeNull();
  });

  it("sends logout with the CSRF token, then forgets it", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-1" });
    await api.ownerLogin("1234");
    reply({ ok: true });
    await api.ownerLogout();
    await api.reportHumanActivity();

    expect(csrfOf(1)).toBe("csrf-1");
    expect(csrfOf(2)).toBeNull();
  });

  it("forgets the CSRF token when the server ends the session", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-1" });
    await api.ownerLogin("1234");
    reply("expired", 401);
    await expect(api.getTheme()).rejects.toBeInstanceOf(api.UnauthenticatedError);
    reply({ ok: true });
    await api.reportHumanActivity();

    expect(csrfOf(2)).toBeNull();
  });

  it("sends the CSRF token on a multipart restore upload without forcing a JSON content type", async () => {
    const api = await freshApi();
    reply({ csrf_token: "csrf-1" });
    await api.ownerLogin("1234");
    reply({});
    await api.uploadBackup(new File(["-- sql"], "backup.sql"));

    const init = fetchMock.mock.calls[1][1];
    expect(csrfOf(1)).toBe("csrf-1");
    expect(init?.body).toBeInstanceOf(FormData);
    expect(new Headers(init?.headers).get("Content-Type")).toBeNull();
  });
});
