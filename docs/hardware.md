# Hardware

> **v1 (current): serial coin acceptor on the server's COM port.** See
> [ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md). The ESP32 /
> HTTP bridge is deferred — see the appendix at the bottom.

> **Customer PC kiosk hardening** (Assigned Access / Shell Launcher v2, the
> separate owner admin account, and the Ctrl+Alt+Del caveat) is part of
> per-machine provisioning, not coin hardware — see
> [installation.md](installation.md#client-kiosk-hardening-assigned-access--shell-launcher-v2)
> and [ADR-0028](adr/0028-client-kiosk-os-hardening.md).

## Current implementation status

- The `CoinSource` boundary has real (`SerialCoinSource`) and simulator
  (`SimulatorCoinSource`) implementations in
  `apps/admin/src-tauri/src/coin_source.rs`. The serial source's dedicated
  **Pulse Sampler** polls the Coin Pin at approximately 30 ms, a conservative
  default for typical pulse coin acceptors, not a bench-validated calibration.
  Pulses narrower than this period may be missed. The first normalized sample seeds the sampler's
  previous-level state without creating an edge or pulse; later rising-edge
  detection writes to a shared buffer. Reset and port recovery use the same
  unseeded state. The 500 ms selection loop drains
  that buffer and performs gap batching, pulse-sum valuation, and commits. The simulator
  injects pulses and starts no sampler thread.
- **DTR is asserted high on open** (initial and every recovery reopen) whenever
  it is not the configured set pin. Field probing (2026-07-25) showed this
  acceptor wiring powers the coin-pin pull-up from the DTR line. Without it the coin line
  floats and chatters at ~200 Hz — phantom credits, and real coin pulses never
  register; with DTR high the line is silent at idle and coins read cleanly
  (₱1→1, ₱5→5, ₱10→10 pulses, and a candidate ₱20→20 pulses; each a
  ~100 ms burst). See `assert_pullup_dtr`.
- The Set Pin is **fail-closed** at construction, on every Selection Window
  close, on port failure, and on `Drop`. A rising edge while it is inhibited is
  admitted only when a Coin Batch is already buffered, so a straddling train
  remains one batch through close drain. An edge observed with an empty buffer
  is refused and logged as a rate-limited **Inhibited Pulse** anomaly. A closed
  batch carries a **Coin Trace Id** through retries and into Audit Log metadata.
  The real source publishes availability edges and bounded sampling anomaly
  aggregates through a narrow best-effort channel; an Admin consumer appends
  `coin_device_unavailable`, `coin_device_recovered`, or
  `coin_sampling_anomaly`. Bridge failures are diagnostic-only and never stop
  sampling or replay a transition. The simulator publishes none of these
  hardware events.
- SQLite-backed coin-device settings (COM port name, set/coin line pair,
  inter-coin gap, retained Pulse Map, and Selection Window timers) plus the
  "test acceptor" action are live. Runtime Coin Source configuration excludes
  the Pulse Map: automatic authority is fixed at 100 cents per observed pulse.
  Migrations 0007, 0008, and 0022 preserve and apply the configuration; the
  field-informed default gap is 600 ms.

## Customer PC owner controls

These controls apply only to the Windows Client machines, not the serial coin
hardware. Both are exposed in the Client's Owner PIN-gated Settings:

- **Launch at startup** is enabled by default and registers the Client for the
  Windows account that changes the setting. It must be enabled for the kiosk
  account and verified after a sign-in; it is not a machine-wide service or a
  watchdog.
- **Disable Windows sign-out** is disabled by default. When enabled, it removes
  Sign out, Switch user, and Win+L while deliberately retaining Ctrl+Alt+Del
  Restart and Shut down. The policy write requests one UAC consent. Declining
  consent leaves the previous state active and the owner should confirm the
  toggle remains unchanged.

Before deployment, verify the separate Windows administrator account and Owner
PIN recovery path. If the policy leaves the kiosk account inaccessible, use the
administrator account and the registry cleanup commands in
[installation.md](installation.md#disable-windows-sign-out); never remove the
Restart/Shut down recovery options during provisioning.

## Conceptual flow (v1)

```text
Coin acceptor (coin pin)  -> server COM status line  -> admin process counts pulses
Admin process (set pin)   -> server COM control line -> enable/inhibit the acceptor
              -> active terminal selection supplies the pc_id
              -> session-aware SQLite transaction (existing coin path)
              -> authoritative PC/account time and points -> client timer
```

There is one physical acceptor for the whole shop, wired to the server PC that
runs the admin process and the customer **Central Terminal** kiosk. The
acceptor never knows which PC was chosen; the server's **terminal selection**
supplies the `pc_id`, and the server resolves account-vs-guest routing from the
PC's live login state (ADR-0008). The acceptor and firmware must never
calculate or submit `seconds_added` or points.

## The two COM-header lines

A PC serial port exposes control **outputs** the software can drive (RTS, DTR)
and status **inputs** the software can read (CTS, DSR, RI, DCD). OpenPiso uses
one of each:

| Pin (label) | Direction | COM line (typical) | Role |
|---|---|---|---|
| **Coin pin** (blue) | acceptor → PC (read) | a status input (e.g. CTS/DSR/RI/DCD) | one or more pulses per inserted coin |
| **Set pin** (green) | PC → acceptor (drive) | a control output (e.g. RTS/DTR) | enable (assert) / inhibit (deassert) the acceptor |

The exact line for each pin is an owner-editable setting (calibrated per
install), not hard-coded.

## Selection-first lock (set pin is fail-closed)

A physical acceptor ingests the coin *before* it emits a pulse, so an
"unrouted" coin cannot be refused after the fact. OpenPiso prevents that case by
keeping the **set pin inhibited** unless a customer has an open selection:

- inhibited at process start (before the serial port is even opened),
- inhibited whenever no Selection Window is open,
- inhibited on crash/restart (the in-memory selection is gone) and on shutdown,
- asserted **only** while a Selection Window is open.

A train that started while asserted may straddle inhibition: the sampler admits
its later edges while the pulse buffer is non-empty, and the close drain credits
the full Coin Batch to the just-selected PC. An edge with an empty buffer stays
refused as an **Inhibited Pulse**. Shop Hours also gates opening a new
Selection Window in Rust while the shop is closed. The acceptor is therefore never energised and a coin falls through to
its return slot. A Selection Window that was already open when Shop Status
became Shop Closed is allowed to finish naturally; its coins remain credited to
that selected PC.

Worst case is that a customer must re-select before inserting — never free play,
never a silently swallowed coin.

## Pulses → pesos (gap-batched pulse sum)

The acceptor emits pulses on the coin pin; a quiet **inter-coin gap** closes a
**Coin Batch**. Automatic authority is always checked `pulse_count × 100 cents`,
producing exactly one coin transaction for every positive pulse count. The
persisted Pulse Map is not consulted by runtime acceptance; it remains for
physical calibration evidence and historical Rejection Credit denomination
choices.

```text
inter_coin_gap_ms = 600   # field-informed starting point; bench-validate
automatic_authority = 100 cents per pulse
```

## Coin source abstraction

The admin process reads coins through a `CoinSource` boundary with two
implementations:

- **Serial** — the real acceptor on the COM port.
- **Simulator** — for hardware-free development/testing; it injects a coin onto
  the **currently active selection's PC**, exercising the same selection, lock,
  and commit code as real hardware.

If the serial source cannot open the port or drops mid-operation, the terminal
shows "coin acceptor unavailable / out of order" and refuses to open a Selection
Window (fail-closed). After 20 consecutive Coin Pin read failures, the Pulse
Sampler drops the handle and retries reopening automatically with a bounded
1–30 second exponential backoff. Reopen always inhibits the Set Pin first;
recovery never enables the acceptor or carries pulses across the failed handle.
Each real availability edge produces at most one operational Audit Log Notice;
retries do not. Set Pin assertion/inhibition failures use the same edge
boundary.

## Bench testing (v1)

1. Plug in the serial adapter. In `:8080/admin` → coin-device settings,
   press **Refresh**, select the port that appeared, then configure the
   set/coin line mapping, gap, and pulse map. Save and use **Test acceptor**.
   The saved port remains selectable when temporarily unplugged and is shown
   as not detected; an empty COM Port explicitly selects the simulator.
2. Verify the set pin is inhibited at idle (no coins accepted).
3. With Shop Hours enabled and Shop Status Shop Closed, confirm opening a
   selection is refused and the set pin stays inhibited; any inserted coin is
   physically returned.
4. Open a selection for a guest PC while Shop Open; confirm the set pin asserts
   and one coin's observed pulse count is valued at 100 cents per pulse by the
   active Shop Rate, with one transaction + one audit record.
5. Repeat with an account logged in on the selected PC; confirm the account (not
   the PC Guest Time) receives the time and eligible points (ADR-0008).
6. Confirm the window closes on inactivity / `Enter` / hard cap and the set pin
   re-inhibits.
7. For a physical ₱20 calibration, save the serial settings, then use the
   inline owner-only **₱20 Calibration** flow. It captures five individual
   genuine ₱20 coins under an exclusive serial-source lock; it creates no
   transaction. Apply only when all five pulse counts agree. A conflicting
   pulse count is never overwritten automatically.
8. Record five individual ₱20 trials, three normal ₱10 trials, and one rapid
   two-₱10 trial with the configured acceptor. Verify RTS inhibit wiring and
   ensure only one application owns the COM port.
9. Derive the production inter-coin gap from observed intra-coin gaps with
   margin. Do not change the global 600 ms default based on one capture alone;
   the rapid two-₱10 result documents accepted batch semantics and never
   changes settings automatically.
10. Restart/reconfigure Admin after the bench checks and verify the observed ₱20
    pulse count through the authoritative Selection Window path. Confirm the
    persisted transaction is pulse count × 100 cents and its seconds are
    server-calculated; the retained Pulse Map value must not affect it.

## Conceptual wiring (electrical)

Coin acceptors vary. Check the specific acceptor's datasheet before applying
power. The coin pulse output and the inhibit/enable input typically run at the
acceptor's logic level (often 12 V or 5 V), which must be level-shifted /
optically isolated to the PC serial port's levels — do **not** wire acceptor
voltages directly to COM-header pins.

```text
12 V supply +     ---- coin acceptor V+
12 V supply -     ---- coin acceptor GND
coin pulse output ---- isolation/level shift ---- COM status input (coin pin)
COM control output ---- isolation/level shift ---- acceptor inhibit input (set pin)
common ground only where the chosen interface requires it
```

---

## Appendix: deferred ESP32 / HTTP bridge

The original design used an ESP32 that counts the acceptor's pulses, maps a
completed batch to integer cents, and sends `POST /api/coin-events` with the
selected `pc_id` over the LAN. This path is **deferred** in favour of the
direct serial input above, but the server endpoint remains for the simulator and
for future networked devices.

Device validation (when revived): OpenPiso seeds only the `dev-simulator`
device. A physical bridge needs its own `coin_devices` row (unique id, locally
generated secret, `active = 1`); the server compares the `signature` field and
rejects missing/unknown/inactive/mismatched devices with HTTP 401. That is basic
validation, not production authentication — production hardware requires
HMAC-SHA256 over a canonical payload, a nonce/timestamp replay window, secret
provisioning/rotation, and preferably TLS or an isolated trusted network.
