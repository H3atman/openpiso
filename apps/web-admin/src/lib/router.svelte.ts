import { IsMobile, mobileMediaQuery } from "$lib/hooks/is-mobile.svelte.js";
import {
  parseRoute,
  resolveRoute,
  restoreRoute,
  routePath,
  type Route,
} from "./routing";

const LAST_ROUTE_KEY = "openpiso.last-route";

function isBarePath(pathname: string, page: string): boolean {
  return pathname === `/${page}` || pathname === `/${page}/`;
}

export class BrowserRouter {
  route = $state<Route>(parseRoute(window.location.pathname));
  // True when the address being resolved was the bare Settings path.
  // ADR-0065 renders that path as a section index below 768px; SettingsPage
  // derives the index from this flag and the shared mobile media-query hook.
  bareSettings = $state(false);
  #isMobile = new IsMobile();
  #mobileQuery = window.matchMedia(mobileMediaQuery());
  #active = false;
  #scrollContainer: HTMLElement | undefined;

  #sync = () => {
    const pathname = window.location.pathname;
    const bareAddress = isBarePath(pathname, "admin");
    const bareSettings = isBarePath(pathname, "admin/settings");
    this.bareSettings = bareSettings;
    // ADR-0065: below 768px the bare Settings path renders a section index
    // instead of resolving to Points, so keep the bare URL (the index must
    // survive reloads) and record no section the owner never chose.
    const settingsIndexShown = bareSettings && this.#isMobile.current;
    let restoredRoute: Route | null = null;
    if (bareAddress) {
      try {
        restoredRoute = restoreRoute(
          localStorage.getItem(LAST_ROUTE_KEY),
          Date.now(),
        );
      } catch {
        // Storage may be unavailable; Dashboard remains the safe default.
      }
    }

    const resolution = restoredRoute
      ? { route: restoredRoute, replacementPath: routePath(restoredRoute) }
      : bareAddress
        ? {
            route: { page: "dashboard" } as const,
            replacementPath: "/admin/dashboard",
          }
        : resolveRoute(pathname);
    if (resolution.replacementPath && !settingsIndexShown) {
      history.replaceState(null, "", resolution.replacementPath);
    }
    this.route = resolution.route;
    this.#scrollContainer?.scrollTo({ top: 0 });

    if (this.route.page !== "not-found" && !settingsIndexShown) {
      try {
        localStorage.setItem(
          LAST_ROUTE_KEY,
          JSON.stringify({ path: routePath(this.route), savedAt: Date.now() }),
        );
      } catch {
        // Navigation still works when storage is unavailable.
      }
    }
  };

  start(): void {
    if (this.#active) return;
    this.#active = true;
    window.addEventListener("popstate", this.#sync);
    // Crossing 768px changes which URL authority the bare Settings path has
    // (ADR-0065), so re-resolve on viewport changes rather than going stale.
    this.#mobileQuery.addEventListener("change", this.#sync);
    this.#sync();
  }

  stop(): void {
    if (!this.#active) return;
    this.#active = false;
    window.removeEventListener("popstate", this.#sync);
    this.#mobileQuery.removeEventListener("change", this.#sync);
  }

  navigate(route: Route): void {
    history.pushState(null, "", routePath(route));
    this.#sync();
  }

  setScrollContainer(element: HTMLElement): void {
    this.#scrollContainer = element;
  }
}
