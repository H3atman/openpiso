# ADR-0006: Session lifecycle — crash, re-login, idle drain

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Refines: ADR-0001 (time ownership), ADR-0002 (single active session),
  ADR-0005 (watchdog restart).

## Context

ADR-0005 leaves open what a watchdog-restarted client shows, since the server
session is still live and the account is still burning. This ADR pins the
session lifecycle.

## Decision

- **A crash/kill is NOT a logout.** The session stays alive and the account
  keeps counting down server-side through a client crash and restart.
- **On restart, force re-login.** The relaunched client shows the login screen;
  the customer must retype username + password. A few seconds of time burn
  during the fumble is accepted. Re-logging in on the *same* PC where the
  account already has an active session simply **resumes** that session (it does
  not start a new one or trigger any auto-kick of itself).
- **Idle drain is intended.** While an account is logged in and the PC is on,
  the account drains every second regardless of activity — holding a seat costs
  time, same as the MVP's PCs counted down while idle. **Logging out is how a
  customer protects their remaining balance.** The client UI must make this
  explicit ("Log out to save your time") so customers don't feel robbed when
  they return to a drained account.

### Session-ending events (the only things that end a logged-in session)
1. Explicit **logout** (balance stays on account, PC → 0 — ADR-0001).
2. **Auto-kick** when the same account logs in at another PC (ADR-0002).
3. **Time = 0** → power-off (ADR-0005).
4. *(open)* **Another account logging in at the same PC** — see "still open".

## Consequences / still open

- **PC-level takeover is undecided.** If a *different* account reaches the login
  screen on a PC that still has account B's session live (e.g. after a crash,
  or via a "switch user" affordance), what happens to B's still-burning
  session and balance is not yet decided. ADR-0002 only covers one account
  across two PCs; this is the mirror (two accounts at one PC). Next ADR.
