<script lang="ts">
  import { onMount } from "svelte";
  import LoginPage from "$lib/pages/LoginPage.svelte";
  import {
    parseRoute,
    routePath,
    settingsSections,
    type Route,
  } from "$lib/routing";
  import { BrowserRouter } from "$lib/router.svelte";
  import { IsMobile } from "$lib/hooks/is-mobile.svelte";
  import { assertUnreachable, lazy } from "$lib/utils";
  import AppSidebar, { type NavItem } from "$lib/components/app-sidebar.svelte";
  import * as Sidebar from "$lib/components/ui/sidebar/index.js";
  import { Separator } from "$lib/components/ui/separator/index.js";
  import BrandLogo from "$lib/components/brand-logo.svelte";
  import { Toaster } from "$lib/components/ui/sonner/index.js";
  import * as api from "$lib/api";
  import { trackHumanActivity } from "$lib/activity";
  import { applyThemeRecord, appliedThemeVersion } from "$lib/theme";
  import LayoutDashboardIcon from "@lucide/svelte/icons/layout-dashboard";
  import SettingsIcon from "@lucide/svelte/icons/settings";
  import ActivityIcon from "@lucide/svelte/icons/activity";
  import TicketIcon from "@lucide/svelte/icons/ticket";
  import SlidersHorizontalIcon from "@lucide/svelte/icons/sliders-horizontal";
  import ArrowLeftRightIcon from "@lucide/svelte/icons/arrow-left-right";
  import ChartColumnIcon from "@lucide/svelte/icons/chart-column";
  import ScrollTextIcon from "@lucide/svelte/icons/scroll-text";
  import UsersIcon from "@lucide/svelte/icons/users";
  import ArchiveIcon from "@lucide/svelte/icons/archive";
  import BellIcon from "@lucide/svelte/icons/bell";
  import NotificationBanner from "$lib/components/NotificationBanner.svelte";
  import type { NotificationItem } from "$lib/types";

  // Route screens load on demand so the login screen — and each first
  // paint after it — ships only the code that screen needs, instead of one
  // bundle containing every admin page.
  const DashboardPage = lazy(() => import("$lib/pages/DashboardPage.svelte"));
  const SettingsPage = lazy(() => import("$lib/pages/SettingsPage.svelte"));
  const VouchersPage = lazy(() => import("$lib/pages/VouchersPage.svelte"));
  const AdjustmentsPage = lazy(() => import("$lib/pages/AdjustmentsPage.svelte"));
  const TransfersPage = lazy(() => import("$lib/pages/TransfersPage.svelte"));
  const ReportsPage = lazy(() => import("$lib/pages/ReportsPage.svelte"));
  const AuditPage = lazy(() => import("$lib/pages/AuditPage.svelte"));
  const NotificationsPage = lazy(() => import("$lib/pages/NotificationsPage.svelte"));
  const AccountsPage = lazy(() => import("$lib/pages/AccountsPage.svelte"));
  const BackupPage = lazy(() => import("$lib/pages/BackupPage.svelte"));
  const DiagnosticsPage = lazy(() => import("$lib/pages/DiagnosticsPage.svelte"));

  let authenticated = $state(false);
  let mainElement = $state<HTMLElement>();
  let unreadNotificationCount = $state(0);
  let activeAlerts = $state<NotificationItem[]>([]);
  let activeAlertsHasMore = $state(false);
  // Shop-wide Diagnostics Mode (ADR-0081): null until the first poll answers,
  // and neither the nav item nor the page asserts a side until it is known.
  let diagnosticsMode = $state<boolean | null>(null);
  // The most recent Diagnostics Mode read failure, kept alongside the last
  // successful value so a transient poll failure neither flickers the
  // navigation nor leaves the direct Diagnostics route saying "Loading".
  let diagnosticsModeError = $state("");
  // Bumped by every Diagnostics Mode answer, poll or toggle, so a poll that
  // was already in flight when a local toggle landed drops its own, older
  // answer instead of flipping the flag back (ADR-0081).
  let diagnosticsModeGeneration = 0;
  const router = new BrowserRouter();
  const isMobile = new IsMobile();

  $effect(() => {
    if (mainElement) router.setScrollContainer(mainElement);
  });

  const navItems = $derived<NavItem[]>([
    { id: "/admin/dashboard", title: "Dashboard", icon: LayoutDashboardIcon },
    { id: "/admin/vouchers", title: "Vouchers", icon: TicketIcon, group: "Money" },
    {
      id: "/admin/adjustments",
      title: "Adjustments",
      icon: SlidersHorizontalIcon,
      group: "Money",
    },
    {
      id: "/admin/transfers",
      title: "Transfers",
      icon: ArrowLeftRightIcon,
      group: "Money",
    },
    { id: "/admin/accounts", title: "Accounts", icon: UsersIcon, group: "People" },
    { id: "/admin/reports", title: "Sales Report", icon: ChartColumnIcon, group: "Records" },
    { id: "/admin/audit", title: "Audit", icon: ScrollTextIcon, group: "Records" },
    { id: "/admin/backups", title: "Backups", icon: ArchiveIcon, group: "System" },
    {
      id: "/admin/notifications",
      title: "Notifications",
      icon: BellIcon,
      group: "System",
      badge: unreadNotificationCount > 0 ? unreadNotificationCount : undefined,
    },
    {
      id: "/admin/settings",
      title: "Settings",
      icon: SettingsIcon,
      group: "System",
      children: settingsSections.map((section) => ({
        id: `/admin/settings/${section.id}`,
        title: section.label,
      })),
    },
    // Directly after Settings in the System group (ADR-0081), and only while
    // Diagnostics Mode is on — the flag is visibility, so the sidebar item is
    // visibility too.
    ...(diagnosticsMode === true
      ? [
          {
            id: "/admin/diagnostics",
            title: "Diagnostics",
            icon: ActivityIcon,
            group: "System",
          },
        ]
      : []),
  ]);

  const activeNavId = $derived(
    // The Settings index (ADR-0065) names no section, so highlight the
    // Settings branch itself rather than the resolved Points fallback.
    router.bareSettings && isMobile.current
      ? "/admin/settings"
      : routePath(router.route),
  );
  const activeTitle = $derived(
    router.route.page === "settings"
      ? "Settings"
      : router.route.page === "diagnostics"
        ? "Diagnostics"
        : router.route.page === "not-found"
          ? "Not found"
          : navItems.find((item) => item.id === activeNavId)?.title ?? "OpenPiso",
  );

  function selectNav(event: MouseEvent, path: string) {
    if (
      event.button !== 0 ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey ||
      event.altKey
    ) return;

    event.preventDefault();
    router.navigate(parseRoute(path));
  }

  async function handleLogout() {
    try {
      await api.ownerLogout();
    } catch {
      // Swallow — we clear local state regardless.
    }
    router.stop();
    authenticated = false;
    unreadNotificationCount = 0;
    activeAlerts = [];
    activeAlertsHasMore = false;
  }

  // Root-layout theme poller (ADR-0035): re-apply only when the server's
  // version moves. The tab that PUTs applies immediately from its own
  // response; other tabs follow within one poll interval.
  //
  // `GET /api/theme` requires an owner session, so a 401 means this tab's
  // session is gone (the admin app restarted — owner tokens are in-memory) even
  // though the cookie survives. Drop to the login screen rather than polling a
  // dead session behind a logged-in-looking UI.
  async function pollTheme() {
    try {
      const record = await api.getTheme();
      if (record.version !== appliedThemeVersion()) {
        applyThemeRecord(record);
      }
    } catch (error) {
      if (error instanceof api.UnauthenticatedError) {
        router.stop();
        authenticated = false;
        return;
      }
      // Unreachable server — keep the last-applied theme.
    }
  }

  async function pollNotifications() {
    try {
      const [count, alerts] = await Promise.all([
        api.getNotificationUnreadCount(),
        api.listNotifications({ limit: 50, offset: 0, severity: "alert", unread: true }),
      ]);
      unreadNotificationCount = count.unread_count;
      activeAlerts = alerts.items;
      activeAlertsHasMore = alerts.has_more;
    } catch (error) {
      if (error instanceof api.UnauthenticatedError) {
        router.stop();
        authenticated = false;
      }
      // Preserve the last successful state on transient failures.
    }
  }

  // One authenticated root poll owns live theme, notification, and Diagnostics
  // Mode state. A flag flipped from another device (or from Settings →
  // Security) reaches this tab's sidebar and page within one poll interval.
  // The login screen has no session, so it never polls protected endpoints.
  $effect(() => {
    if (!authenticated) return;
    const poll = () =>
      void Promise.all([pollTheme(), pollNotifications(), pollDiagnosticsMode()]);
    poll();
    const rootPoll = setInterval(poll, 15_000);
    return () => clearInterval(rootPoll);
  });

  // Real pointer, keyboard, and touch input is the only thing that keeps the
  // Owner Session from idling out (ADR-0074); the polls above never count.
  async function reportHumanActivity() {
    try {
      await api.reportHumanActivity();
    } catch (error) {
      if (error instanceof api.UnauthenticatedError) {
        router.stop();
        authenticated = false;
      }
    }
  }

  $effect(() => {
    if (!authenticated) return;
    return trackHumanActivity(() => void reportHumanActivity());
  });

  async function pollDiagnosticsMode() {
    const generation = ++diagnosticsModeGeneration;
    try {
      const { enabled } = await api.getDiagnosticsMode();
      if (generation !== diagnosticsModeGeneration) return;
      diagnosticsMode = enabled;
      diagnosticsModeError = "";
    } catch (error) {
      if (error instanceof api.UnauthenticatedError) {
        router.stop();
        authenticated = false;
        return;
      }
      if (generation !== diagnosticsModeGeneration) return;
      // Unreachable server: keep the last-known flag and name the failure, so
      // a page that has no value yet can say so instead of loading forever.
      diagnosticsModeError = error instanceof Error ? error.message : String(error);
    }
  }

  // One browser, one Diagnostics Mode value: a local toggle publishes the
  // server's answer here immediately, so the sidebar, the Diagnostics page,
  // and the Coin Device pointer move together. Changes made on another device
  // still arrive through the poll above.
  function publishDiagnosticsMode(enabled: boolean) {
    // The server just answered a write, so this supersedes any read in flight.
    diagnosticsModeGeneration += 1;
    diagnosticsMode = enabled;
    diagnosticsModeError = "";
  }

  onMount(() => {
    router.start();

    void (async () => {
      try {
        const session = await api.getSession();
        if (session.authenticated) {
          authenticated = true;
        }
      } catch {
        // Not authenticated — show login.
      }
    })();

    return () => router.stop();
  });
</script>

<Toaster position="bottom-right" swipeDirections={["right"]} expand={false} visibleToasts={3} />

{#if !authenticated}
  <LoginPage
    onLogin={() => {
      authenticated = true;
      router.start();
    }}
  />
{:else}
  <Sidebar.Provider>
    <AppSidebar
      items={navItems}
      activeId={activeNavId}
      onSelect={selectNav}
      onLogout={handleLogout}
      user={{ name: "Owner", email: "Pisonet admin" }}
    />
    <Sidebar.Inset>
      <header
        class="flex h-16 shrink-0 items-center gap-2 border-b transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12"
      >
        <div class="flex items-center gap-2 px-4">
          <Sidebar.Trigger class="-ms-1" />
          <Separator
            orientation="vertical"
            class="me-2 data-[orientation=vertical]:h-4"
          />
          <div class="md:hidden">
            <BrandLogo variant="symbol" class="size-6 shrink-0" />
          </div>
          <h1 class="text-base font-medium">{activeTitle}</h1>
        </div>
      </header>
      <main
        class="flex-1 overflow-y-auto p-4 md:p-6"
        bind:this={mainElement}
      >
        <NotificationBanner
          alerts={activeAlerts}
          hasMore={activeAlertsHasMore}
          onNavigate={() => router.navigate({ page: "notifications" })}
        />
        {#if router.route.page === "dashboard"}
          {#await DashboardPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "settings"}
          {#await SettingsPage() then { default: Page }}
            <Page
              section={router.route.section}
              bare={router.bareSettings}
              onSelectSection={(section) =>
                router.navigate({ page: "settings", section })}
              {diagnosticsMode}
              {diagnosticsModeError}
              onDiagnosticsModeChange={publishDiagnosticsMode}
              onRetryDiagnosticsMode={pollDiagnosticsMode}
            />
          {/await}
        {:else if router.route.page === "vouchers"}
          {#await VouchersPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "adjustments"}
          {#await AdjustmentsPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "transfers"}
          {#await TransfersPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "reports"}
          {#await ReportsPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "audit"}
          {#await AuditPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "notifications"}
          {#await NotificationsPage() then { default: Page }}
            <Page onNotificationsChanged={pollNotifications} />
          {/await}
        {:else if router.route.page === "accounts"}
          {#await AccountsPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "backups"}
          {#await BackupPage() then { default: Page }}
            <Page />
          {/await}
        {:else if router.route.page === "diagnostics"}
          {#await DiagnosticsPage() then { default: Page }}
            <Page
              enabled={diagnosticsMode}
              loadError={diagnosticsModeError}
              onRetry={pollDiagnosticsMode}
            />
          {/await}
        {:else if router.route.page === "not-found"}
          <div class="rounded-lg border p-6">
            <h2 class="text-lg font-semibold">Not found</h2>
            <p class="mt-2 text-sm text-muted-foreground">This screen does not exist.</p>
          </div>
        {:else}
          <!-- A screen with no branch above narrows to `never` here, so adding
               one without rendering it fails to compile rather than silently
               falling through to Not found. -->
          {assertUnreachable(router.route.page)}
        {/if}
      </main>
    </Sidebar.Inset>
  </Sidebar.Provider>
{/if}
