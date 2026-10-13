import { describe, expect, test } from "vitest";
import {
  formatAuditSummary,
  formatAttendanceDeadline,
  formatBonusPercent,
  formatCentsAsPesosInput,
  formatClockDifference,
  formatDayDuration,
  formatMoney,
  formatMoneyCompact,
  formatFractionalSeconds,
  formatProseDuration,
  formatSignedDuration,
  formatSignedProseDuration,
  formatSalesDateRange,
  guardCoverageLabel,
  incidentKindHint,
  parsePesosToCents,
  exitReasonLabel,
  statusLabel,
} from "./format";

describe("prose duration", () => {
  test("drops zero units and spells out hours", () => {
    expect(formatProseDuration(0)).toBe("0 sec");
    expect(formatProseDuration(30)).toBe("30 sec");
    expect(formatProseDuration(2_700)).toBe("45 min");
    expect(formatProseDuration(5_400)).toBe("1 hour 30 min");
    expect(formatProseDuration(90_000)).toBe("25 hours");
    expect(formatFractionalSeconds(1, 5)).toBe("0.20 sec");
    expect(formatFractionalSeconds(1, 3)).toBe("0.333333… sec");
  });

  test("formats Attendance audit deltas with signed prose durations", () => {
    expect(formatSignedProseDuration(3_600)).toBe("+1 hour");
    expect(formatSignedProseDuration(-1_800)).toBe("-30 min");
    expect(formatSignedProseDuration(0)).toBe("0 sec");
  });
});

describe("signed duration", () => {
  test("formats duration deltas with direction and no sign for zero", () => {
    expect(formatSignedDuration(-200)).toBe("-00:03:20");
    expect(formatSignedDuration(200)).toBe("+00:03:20");
    expect(formatSignedDuration(0)).toBe("00:00:00");
  });

  test("describes the direction of a Client clock difference", () => {
    expect(formatClockDifference(90_000)).toBe("1 min 30 sec behind the Admin clock");
    expect(formatClockDifference(-90_000)).toBe("1 min 30 sec ahead of the Admin clock");
  });
});

describe("batch day duration", () => {
  test("formats seconds as DD:HH:mm:ss", () => {
    expect(formatDayDuration(2 * 86_400 + 3 * 3_600 + 4 * 60 + 5)).toBe(
      "02:03:04:05",
    );
  });

  test("renders the peso sign as U+20B1", () => {
    expect(formatMoney(5_000)).toContain("\u20b1");
  });

  test("shortens axis money only when there are no cents to drop", () => {
    expect(formatMoneyCompact(100)).toBe("₱1");
    expect(formatMoneyCompact(50_000)).toBe("₱500");
    expect(formatMoneyCompact(100_000)).toBe("₱1k");
    expect(formatMoneyCompact(250_000)).toBe("₱2.5k");
    expect(formatMoneyCompact(50)).toBe(formatMoney(50));
  });

  test("parses peso input exactly into cents", () => {
    expect(parsePesosToCents("50")).toBe(5_000);
    expect(parsePesosToCents("50.5")).toBe(5_050);
    expect(parsePesosToCents("50.505")).toBeNull();
  });

  test("formats server cents as plain peso input and Attendance bonus percent", () => {
    expect(formatCentsAsPesosInput(1_000)).toBe("10");
    expect(formatCentsAsPesosInput(1_050)).toBe("10.5");
    expect(formatBonusPercent(12_000)).toBe("120.00%");
  });

  test("formats the inclusive Admin-local claim deadline without timezone conversion", () => {
    expect(formatAttendanceDeadline("2026-10-31")).toBe("Oct 31, 2026");
  });

  test("formats multi-word Attendance states consistently", () => {
    expect(statusLabel("not_yet_claimable")).toBe("Not Yet Claimable");
  });

  test("formats inclusive sales ranges without repeating same-month values", () => {
    expect(formatSalesDateRange("2026-08-06", "2026-08-06")).toBe(
      "Aug 6, 2026",
    );
    expect(formatSalesDateRange("2026-08-03", "2026-08-09")).toBe(
      "Aug 3–9, 2026",
    );
    expect(formatSalesDateRange("2026-01-01", "2026-08-06")).toBe(
      "Jan 1–Aug 6, 2026",
    );
    expect(formatSalesDateRange(null, null)).toBe("All time");
  });

  test("attributes an account actor when it differs from the target by id", () => {
    expect(
      formatAuditSummary({
        summary: "Sent 1min 0sec",
        actor: "account",
        actor_account_id: "actor-id",
        actor_account_username: "juan",
        account_id: "target-id",
      }),
    ).toBe("Sent 1min 0sec — by juan");
  });

  test("suppresses attribution when actor and target ids match", () => {
    expect(
      formatAuditSummary({
        summary: "Authenticated",
        actor: "account",
        actor_account_id: "same-id",
        actor_account_username: "juan",
        account_id: "same-id",
      }),
    ).toBe("Authenticated");
  });

  test("renders an unknown account when the actor id does not resolve", () => {
    expect(
      formatAuditSummary({
        summary: "Sent 1min 0sec",
        actor: "account",
        actor_account_id: "missing-id",
        actor_account_username: null,
        account_id: "target-id",
      }),
    ).toBe("Sent 1min 0sec — by (unknown account)");
  });

  test("does not attribute rows without a recorded actor id", () => {
    expect(
      formatAuditSummary({
        summary: "Login denied",
        actor: "account",
        actor_account_id: null,
        actor_account_username: null,
        account_id: "target-id",
      }),
    ).toBe("Login denied");
  });

  test("attributes an account actor when there is no target account", () => {
    expect(
      formatAuditSummary({
        summary: "Changed password",
        actor: "account",
        actor_account_id: "actor-id",
        actor_account_username: "juan",
        account_id: null,
      }),
    ).toBe("Changed password — by juan");
  });

  test.each(["admin", "system", "coin_device"])(
    "leaves %s actors unchanged",
    (actor) => {
      expect(
        formatAuditSummary({
          summary: "Accepted coin",
          actor,
          actor_account_id: "actor-id",
          actor_account_username: "juan",
          account_id: "target-id",
        }),
      ).toBe("Accepted coin");
    },
  );

  test("keeps the resolved sender on transfer-in summaries", () => {
    expect(
      formatAuditSummary({
        summary: "Received 1min 0sec from audit-sender",
        actor: "account",
        actor_account_id: "sender-id",
        actor_account_username: "audit-sender",
        account_id: "recipient-id",
      }),
    ).toBe("Received 1min 0sec from audit-sender — by audit-sender");
  });
});

describe("guardCoverageLabel", () => {
  test("labels every typed Guard diagnostic without fabricating one", () => {
    expect(guardCoverageLabel("covered")).toBe("Guard: covered");
    expect(guardCoverageLabel("unavailable")).toBe("Guard: unavailable");
    expect(guardCoverageLabel("not_applicable")).toBe("Guard: not applicable");
    // An older Client that never reported the field: unsupported or unknown.
    expect(guardCoverageLabel("unsupported")).toBe(
      "Guard: unsupported or unknown",
    );
  });
});

describe("incidentKindHint", () => {
  test("points Guard guidance at checks on the PC, not automatic repair", () => {
    const hint = incidentKindHint("client_guard_coverage");
    expect(hint).toMatch(/Guard service/);
    expect(hint).toMatch(/Client installation/);
    expect(hint).toMatch(/never repairs services/i);
  });
});

describe("exitReasonLabel", () => {
  test("names a recorded Exit Reason for what it was, never as a crash", () => {
    expect(exitReasonLabel("window_closed")).toBe("Window closed");
    expect(exitReasonLabel("application_exit")).toBe("Application exit");
    // A panic is the one recorded reason that is crash-shaped.
    expect(exitReasonLabel("panic")).toBe("Panic");
  });
});
