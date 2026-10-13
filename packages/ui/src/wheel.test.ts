import { describe, expect, test } from "bun:test";
import {
  chartCssColor,
  chartCssTextColor,
  computeExpectations,
  computeSliceArcs,
  formatPrizeLine,
  labelAnchor,
  rotationForSlice,
  svgArcPath,
} from "./wheel";

const TAU = 2 * Math.PI;

// ---------------------------------------------------------------------------
// Colour cycle
// ---------------------------------------------------------------------------

describe("chartCssColor", () => {
  test("cycles through chart-1 … chart-5 and wraps", () => {
    expect(chartCssColor(0)).toBe("var(--chart-1)");
    expect(chartCssColor(1)).toBe("var(--chart-2)");
    expect(chartCssColor(2)).toBe("var(--chart-3)");
    expect(chartCssColor(3)).toBe("var(--chart-4)");
    expect(chartCssColor(4)).toBe("var(--chart-5)");
    expect(chartCssColor(5)).toBe("var(--chart-1)");
    expect(chartCssColor(6)).toBe("var(--chart-2)");
    expect(chartCssColor(99)).toBe("var(--chart-5)");
  });

  test("uses mode-stable contrasting text across the ramp", () => {
    expect([0, 1, 2].map(chartCssTextColor)).toEqual(Array(3).fill("oklch(0.145 0 0)"));
    expect([3, 4, 8, 9].map(chartCssTextColor)).toEqual(Array(4).fill("oklch(0.985 0 0)"));
  });
});

// ---------------------------------------------------------------------------
// Slice arcs
// ---------------------------------------------------------------------------

describe("computeSliceArcs", () => {
  test("every slice is the same size and they sum to a full turn", () => {
    const arcs = computeSliceArcs(4);
    expect(arcs).toHaveLength(4);
    for (const a of arcs) expect(a.fraction).toBeCloseTo(0.25, 10);
    // First arc starts at top
    expect(arcs[0].startAngle).toBeCloseTo(-Math.PI / 2, 10);
    // Last arc ends one full turn from start
    expect(arcs[3].endAngle - arcs[0].startAngle).toBeCloseTo(TAU, 10);
  });

  test("arcs are contiguous with no gaps or overlaps", () => {
    const arcs = computeSliceArcs(7);
    for (let i = 0; i < arcs.length - 1; i++) {
      expect(arcs[i].endAngle).toBeCloseTo(arcs[i + 1].startAngle, 10);
    }
    const total = arcs.reduce((sum, a) => sum + a.fraction, 0);
    expect(total).toBeCloseTo(1, 10);
  });

  test("no segment count can produce an unequal slice", () => {
    // The property that makes the wheel honest (ADR-0047): geometry is a
    // function of the count alone, so no input can skew one slice.
    for (const count of [1, 2, 3, 5, 12, 24]) {
      const arcs = computeSliceArcs(count);
      expect(arcs).toHaveLength(count);
      for (const a of arcs) expect(a.fraction).toBeCloseTo(1 / count, 10);
    }
  });

  test("Bokya Rate cannot change equal-slice geometry", () => {
    expect(computeSliceArcs(6, 7_500)).toEqual(computeSliceArcs(6, null));
  });

  test("single segment fills the circle", () => {
    const arcs = computeSliceArcs(1);
    expect(arcs).toHaveLength(1);
    expect(arcs[0].fraction).toBeCloseTo(1, 10);
    expect(arcs[0].startAngle).toBeCloseTo(-Math.PI / 2, 10);
    expect(arcs[0].endAngle - arcs[0].startAngle).toBeCloseTo(TAU, 10);
    expect((svgArcPath(arcs[0], 100).match(/ A/g) ?? [])).toHaveLength(2);
  });

  test("empty and negative counts return no arcs rather than throwing", () => {
    expect(computeSliceArcs(0)).toEqual([]);
    expect(computeSliceArcs(-3)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Label anchor
// ---------------------------------------------------------------------------

describe("labelAnchor", () => {
  test("stays within the slice bisector at fixed radius fraction", () => {
    const arcs = computeSliceArcs(3);
    for (const arc of arcs) {
      const anchor = labelAnchor(arc);
      // Distance from centre
      const dist = Math.hypot(anchor.x, anchor.y);
      expect(dist).toBeCloseTo(0.6, 10);
      // Angle matches midAngle (modulo flip)
      const anchorAngle = Math.atan2(anchor.y, anchor.x);
      expect(Math.abs(anchorAngle - arc.midAngle) % (2 * Math.PI)).toBeLessThan(1e-10);
    }
  });

  test("flips left‑hemisphere labels so they are never upside‑down", () => {
    // midAngle > 90° → should be flipped by +180°
    const leftArc = { startAngle: 1, endAngle: 3, midAngle: 2, fraction: 0.318 };
    // midAngle = 2 rad ≈ 114.6° (> 90° → flip to 114.6 + 180 = 294.6°)
    const anchor = labelAnchor(leftArc);
    expect(anchor.angleDeg).toBeCloseTo((2 * 180) / Math.PI + 180, 5);

    // midAngle < -90° → also flipped
    const leftArcNeg = { startAngle: -3, endAngle: -1, midAngle: -2, fraction: 0.318 };
    const anchorNeg = labelAnchor(leftArcNeg);
    // midAngle ≈ -114.6° (< -90° → flip to -114.6 + 180 = 65.4°)
    expect(anchorNeg.angleDeg).toBeCloseTo((-2 * 180) / Math.PI + 180, 5);
  });
});

// ---------------------------------------------------------------------------
// Slice landing rotation
// ---------------------------------------------------------------------------

describe("rotationForSlice", () => {
  test("empty arcs use a safe zero rotation", () => {
    expect(rotationForSlice(0, [])).toBe(0);
  });
  test("each slice index lands its midpoint at 12 o'clock after rotation", () => {
    const arcs = computeSliceArcs(3);
    for (let i = 0; i < arcs.length; i++) {
      const rot = rotationForSlice(i, arcs);
      const adjusted = arcs[i].midAngle + rot;
      // Normalise to [0, TAU) for comparison
      const normalised = ((adjusted % TAU) + TAU) % TAU;
      const target = ((-Math.PI / 2) % TAU + TAU) % TAU;
      expect(normalised).toBeCloseTo(target, 10);
    }
  });

  test("recomputing arcs from the same weights gives the same rotation", () => {
    const arcs = computeSliceArcs(3);
    const rot1 = rotationForSlice(0, arcs);
    const rot2 = rotationForSlice(0, computeSliceArcs(3));
    expect(rot1 % TAU).toBeCloseTo(rot2 % TAU, 10);
  });

  test("added full turns do not change the landing", () => {
    const arcs = computeSliceArcs(3);
    for (let i = 0; i < arcs.length; i++) {
      const rot = rotationForSlice(i, arcs);
      const mid = arcs[i].midAngle;
      // Without extra turns
      const landing = ((mid + rot) % TAU + TAU) % TAU;
      // With one extra full turn clockwise
      expect(((mid + rot + TAU) % TAU + TAU) % TAU).toBeCloseTo(landing, 10);
      // With one extra full turn anticlockwise
      expect(((mid + rot - TAU) % TAU + TAU) % TAU).toBeCloseTo(landing, 10);
    }
  });
});

// ---------------------------------------------------------------------------
// Expectations
// ---------------------------------------------------------------------------

describe("computeExpectations", () => {
  test("is the plain mean of the segment prizes", () => {
    const segments = [
      { seconds: 300, points_centi: 50, free_spin: false },
      { seconds: 60, points_centi: 10, free_spin: false },
    ];
    const exp = computeExpectations(segments, 0);
    expect(exp.expectedSecondsPerSpin).toBeCloseTo(180, 10);
    expect(exp.expectedPointsPerSpin).toBeCloseTo(30, 10);
  });

  test("repeating a prize is what shifts the expectation", () => {
    // Three losing slices and one 400s slice: a 25% chance of the prize.
    const segments = [
      { seconds: 0, points_centi: 0, free_spin: false },
      { seconds: 0, points_centi: 0, free_spin: false },
      { seconds: 0, points_centi: 0, free_spin: false },
      { seconds: 400, points_centi: 0, free_spin: false },
    ];
    expect(computeExpectations(segments, 0).expectedSecondsPerSpin).toBeCloseTo(100, 10);
  });

  test("combination free-spin prizes and capped chain contribute to both axes", () => {
    const segments = [
      { seconds: 60, points_centi: 50, free_spin: true },
      { seconds: 120, points_centi: 100, free_spin: false },
    ];
    const exp = computeExpectations(segments, 2);
    expect(exp.expectedSecondsPerSpin).toBeCloseTo(157.5, 10);
    expect(exp.expectedPointsPerSpin).toBeCloseTo(131.25, 10);
  });

  test("matches Rust's shared biased-distribution fixture", () => {
    const segments = [
      { seconds: 0, points_centi: 0, free_spin: false },
      { seconds: 0, points_centi: 0, free_spin: true },
      { seconds: 400, points_centi: 200, free_spin: false },
    ];

    const exp = computeExpectations(segments, 2, 5_000);

    expect(exp.probabilityBasisPoints).toEqual([5_000, 2_500, 2_500]);
    expect(exp.probabilityPercent).toEqual([50, 25, 25]);
    expect(exp.probabilityBasisPoints.reduce((sum, probability) => sum + probability, 0)).toBe(
      10_000,
    );
    expect(exp.expectedSecondsPerSpin).toBeCloseTo(131.25, 8);
    expect(exp.expectedPointsPerSpin).toBeCloseTo(65.625, 8);
  });

  test("matches Rust's shared unset fixture — the seeded 12-slice default", () => {
    // Shared with default_wheel_validates_and_keeps_a_positive_house_edge in
    // crates/openpiso-core/src/wheel.rs: Try again ×5, 5 min ×3, 10 min ×2,
    // 15 min ×1, Free spin ×1 with a cap of 1. Twelve slices do not divide
    // 10000, so this is where an unset rate rounded to basis points would
    // diverge from the server.
    const make = (n: number, seconds: number, free_spin = false) =>
      Array.from({ length: n }, () => ({ seconds, points_centi: 0, free_spin }));
    const segments = [
      ...make(5, 0),
      ...make(3, 300),
      ...make(2, 600),
      ...make(1, 900),
      ...make(1, 0, true),
    ];
    expect(segments).toHaveLength(12);
    const exp = computeExpectations(segments, 1);
    expect(exp.expectedSecondsPerSpin).toBeCloseTo(270.8333333333, 8);
    expect(exp.probabilityBasisPoints).toEqual([834, 834, 834, 834, ...Array(8).fill(833)]);
    // The one-basis-point remainder is a rounding of an exactly uniform draw,
    // so what the owner reads is the same 8.33% on every slice (ADR-0049).
    expect(exp.probabilityPercent.map((percent) => percent.toFixed(2))).toEqual(
      Array(12).fill("8.33"),
    );
  });

  test("empty input returns zero expectations", () => {
    expect(computeExpectations([], 3)).toEqual({
      expectedSecondsPerSpin: 0,
      expectedPointsPerSpin: 0,
      probabilityBasisPoints: [],
      probabilityPercent: [],
    });
  });
});

// ---------------------------------------------------------------------------
// Derived prize line
// ---------------------------------------------------------------------------

describe("formatPrizeLine", () => {
  test("names the axis by its unit", () => {
    expect(formatPrizeLine({ seconds: 300, points_centi: 0, free_spin: false })).toBe("5 min");
    expect(formatPrizeLine({ seconds: 0, points_centi: 200, free_spin: false })).toBe("2.00 pts");
    expect(formatPrizeLine({ seconds: 600, points_centi: 250, free_spin: false })).toBe(
      "10 min + 2.50 pts",
    );
  });

  test("compacts durations without losing the unit", () => {
    expect(formatPrizeLine({ seconds: 45, points_centi: 0, free_spin: false })).toBe("45s");
    expect(formatPrizeLine({ seconds: 3600, points_centi: 0, free_spin: false })).toBe("1h");
    expect(formatPrizeLine({ seconds: 5400, points_centi: 0, free_spin: false })).toBe("1h 30m");
  });

  test("distinguishes winning nothing from a free spin", () => {
    expect(formatPrizeLine({ seconds: 0, points_centi: 0, free_spin: false })).toBe("—");
    expect(formatPrizeLine({ seconds: 0, points_centi: 0, free_spin: true })).toBe("Free spin");
  });

  test("a prize that also respins does not hide the prize", () => {
    expect(formatPrizeLine({ seconds: 300, points_centi: 0, free_spin: true })).toBe("5 min + spin");
  });
});
