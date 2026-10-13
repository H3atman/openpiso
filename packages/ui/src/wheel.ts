// ---------------------------------------------------------------------------
// Wheel geometry — pure, dependency-free functions for Spin-the-Wheel.
// Arc maths, label placement, rotation to land a chosen slice under the
// pointer, and the theme-chart colour cycle.
// ---------------------------------------------------------------------------

/// One slice's angular span and proportion.
export interface WheelArc {
  /** Start angle in radians, measured from the positive x‑axis. */
  startAngle: number;
  /** End angle in radians, measured from the positive x‑axis. */
  endAngle: number;
  /** Midpoint angle in radians. */
  midAngle: number;
  /** Fraction of the full circle (0 … 1). */
  fraction: number;
}

/// Label position & rotation inside a slice, expressed as fractions of
/// the wheel radius so the consumer can scale them to any viewBox.
export interface WheelLabelAnchor {
  /** X position as a fraction of radius from the centre (−1 … 1). */
  x: number;
  /** Y position as a fraction of radius from the centre (−1 … 1). */
  y: number;
  /** CSS `rotate()` value in degrees, oriented for left‑to‑right reading. */
  angleDeg: number;
}

/// Result of `computeExpectations`.
export interface WheelExpectation {
  expectedSecondsPerSpin: number;
  expectedPointsPerSpin: number;
  /** Whole basis points, summing to 10000 — mirrors the server draw contract. */
  probabilityBasisPoints: number[];
  /**
   * Exact landing probability per slice, as a percentage. Display uses this
   * rather than the basis points: integer division leaves a remainder of at
   * most one basis point on the lowest indices of a group, and that split is
   * deliberately not owner-visible (ADR-0049).
   */
  probabilityPercent: number[];
}

// ---------------------------------------------------------------------------
// Colour cycle
// ---------------------------------------------------------------------------

const CHART_TOKENS = [
  "chart-1",
  "chart-2",
  "chart-3",
  "chart-4",
  "chart-5",
] as const;

/// Return the CSS custom‑property reference for the `index`-th slice,
/// cycling through the theme's chart ramp. Consumers apply this as an
/// inline `fill` / `stroke` (or via a style attribute) inside the
/// theme‑aware document.
export function chartCssColor(index: number): string {
  return `var(--${CHART_TOKENS[index % CHART_TOKENS.length]})`;
}

/** Neutral text chosen from the fixed-lightness chart ramp: dark on the three
 * lighter stops, light on the two darker stops. Stable in both theme modes. */
export function chartCssTextColor(index: number): string {
  return index % CHART_TOKENS.length < 3
    ? "oklch(0.145 0 0)"
    : "oklch(0.985 0 0)";
}

// ---------------------------------------------------------------------------
// Slice arcs
// ---------------------------------------------------------------------------

/// Compute arcs for `count` equal segments. Segments carry no probability
/// weight (ADR-0047): each occupies exactly `1/count` of the circle. Bokya Rate
/// can change landing odds but never geometry (ADR-0049). The first arc starts at
/// 12 o'clock (−π/2).
export function computeSliceArcs(count: number, _bokyaRateBasisPoints: number | null = null): WheelArc[] {
  const slices = Math.max(0, Math.trunc(count));
  if (slices === 0) return [];

  const fraction = 1 / slices;
  const sweep = fraction * 2 * Math.PI;
  return Array.from({ length: slices }, (_, index) => {
    const start = -Math.PI / 2 + index * sweep;
    return {
      startAngle: start,
      endAngle: start + sweep,
      midAngle: start + sweep / 2,
      fraction,
    };
  });
}

// ---------------------------------------------------------------------------
// Label anchor
// ---------------------------------------------------------------------------

/// Anchor for a label inside `arc`. Returns a position 60 % of the way
/// from centre to rim along the bisector, and a rotation that keeps text
/// left‑to‑right (flips upside‑down labels on the right half).
export function labelAnchor(arc: WheelArc): WheelLabelAnchor {
  const r = 0.6;
  const x = r * Math.cos(arc.midAngle);
  const y = r * Math.sin(arc.midAngle);
  let angleDeg = (arc.midAngle * 180) / Math.PI;
  // Flip right‑hemisphere labels so they read left‑to‑right.
  if (angleDeg > 90 || angleDeg < -90) angleDeg += 180;
  return { x, y, angleDeg };
}

// ---------------------------------------------------------------------------
// Slice landing rotation
// ---------------------------------------------------------------------------

/// Rotation (in radians) to apply so that slice `index` is centred under
/// the pointer at 12 o'clock (−π/2). Rotate the wheel group by this
/// amount (converted to degrees for SVG `transform`).
export function rotationForSlice(
  index: number,
  arcs: readonly WheelArc[],
): number {
  const arc = arcs[index];
  return arc ? -Math.PI / 2 - arc.midAngle : 0;
}

/** SVG path for one slice centred at the origin. Full circles use two arcs
 * because a single SVG arc with coincident endpoints is invisible. */
export function svgArcPath(arc: WheelArc, radius: number): string {
  if (arc.fraction >= 1 - Number.EPSILON) {
    return `M0,${-radius} A${radius},${radius} 0 1 1 0,${radius} A${radius},${radius} 0 1 1 0,${-radius} Z`;
  }
  const x1 = radius * Math.cos(arc.startAngle);
  const y1 = radius * Math.sin(arc.startAngle);
  const x2 = radius * Math.cos(arc.endAngle);
  const y2 = radius * Math.sin(arc.endAngle);
  const large = arc.fraction > 0.5 ? 1 : 0;
  return `M0,0 L${x1},${y1} A${radius},${radius} 0 ${large} 1 ${x2},${y2} Z`;
}

// ---------------------------------------------------------------------------
// Expectations
// ---------------------------------------------------------------------------

/// Compute the expected seconds and points per paid spin from the unsaved form
/// state. Caller provides the raw segment rows (including the current unsaved
/// values) — no server involvement. The optional Bokya Rate biases Bokya and
/// win groups while leaving slice geometry equal (ADR-0049). Mirrors
/// `WheelConfig::segment_probability_basis_points` and `expected_payout_*`.
export function computeExpectations(
  segments: readonly {
    seconds: number;
    points_centi: number;
    free_spin: boolean;
  }[],
  freeSpinCap: number,
  bokyaRateBasisPoints: number | null = null,
): WheelExpectation {
  const total = segments.length;
  if (total === 0) {
    return {
      expectedSecondsPerSpin: 0,
      expectedPointsPerSpin: 0,
      probabilityBasisPoints: [],
      probabilityPercent: [],
    };
  }

  const isBokya = segments.map(
    (segment) =>
      segment.seconds === 0 && segment.points_centi === 0 && !segment.free_spin,
  );
  const probabilityBasisPoints = Array<number>(total).fill(0);
  const probabilityPercent = Array<number>(total).fill(0);
  const assignGroup = (group: boolean, groupTotal: number) => {
    const indices = isBokya.flatMap((bokya, index) =>
      bokya === group ? [index] : [],
    );
    if (indices.length === 0) return;
    const share = Math.trunc(groupTotal / indices.length);
    const remainder = groupTotal % indices.length;
    indices.forEach((index, groupIndex) => {
      probabilityBasisPoints[index] = share + (groupIndex < remainder ? 1 : 0);
      probabilityPercent[index] = groupTotal / indices.length / 100;
    });
  };
  if (bokyaRateBasisPoints === null) {
    const share = Math.trunc(10_000 / total);
    const remainder = 10_000 % total;
    probabilityBasisPoints.forEach((_, index) => {
      probabilityBasisPoints[index] = share + (index < remainder ? 1 : 0);
      probabilityPercent[index] = 100 / total;
    });
  } else {
    assignGroup(true, bokyaRateBasisPoints);
    assignGroup(false, 10_000 - bokyaRateBasisPoints);
  }

  let expectedSecondsSingle = 0;
  let expectedPointsSingle = 0;
  let freeProbability = 0;
  segments.forEach((segment, index) => {
    const probability =
      bokyaRateBasisPoints === null ? 10_000 / total : (probabilityBasisPoints[index] ?? 0);
    expectedSecondsSingle += segment.seconds * probability;
    expectedPointsSingle += segment.points_centi * probability;
    if (segment.free_spin) freeProbability += probability / 10_000;
  });
  expectedSecondsSingle /= 10_000;
  expectedPointsSingle /= 10_000;
  const freeRatio = freeProbability;
  let expectedLandings = 0;
  let term = 1;
  for (let i = 0; i <= Math.max(0, Math.trunc(freeSpinCap)); i += 1) {
    expectedLandings += term;
    term *= freeRatio;
  }
  return {
    expectedSecondsPerSpin: expectedSecondsSingle * expectedLandings,
    expectedPointsPerSpin: expectedPointsSingle * expectedLandings,
    probabilityPercent,
    probabilityBasisPoints,
  };
}

/** Bokya Rate implied by equal slice counts, rounded to one basis point. */
export function naturalBokyaRateBasisPoints(
  segments: readonly {
    seconds: number;
    points_centi: number;
    free_spin: boolean;
  }[],
): number {
  if (segments.length === 0) return 0;
  const bokyaCount = segments.filter(
    (segment) =>
      segment.seconds === 0 && segment.points_centi === 0 && !segment.free_spin,
  ).length;
  return Math.round((bokyaCount / segments.length) * 10_000);
}

// ---------------------------------------------------------------------------
// Derived prize line
// ---------------------------------------------------------------------------

/// A slice's prize, stated by the slice itself. Derived from the segment's own
/// seconds and centipoints, so — unlike the owner's free‑text label — it cannot
/// misdescribe what landing there pays (ADR-0047).
export interface WheelPrizeSegment {
  seconds: number;
  points_centi: number;
  free_spin: boolean;
}

/// Compact duration for a wheel slice: `45s`, `5 min`, `1h`, `1h 30m`. Kept
/// short deliberately — this renders inside a slice, not in a table.
function formatPrizeDuration(totalSeconds: number): string {
  const secs = Math.max(0, Math.floor(totalSeconds));
  if (secs < 60) return `${secs}s`;
  const minutes = Math.floor(secs / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const remainder = minutes % 60;
  return remainder === 0 ? `${hours}h` : `${hours}h ${remainder}m`;
}

/// Points with the two decimals ADR-0038 requires everywhere.
function formatPrizePoints(pointsCenti: number): string {
  const abs = Math.abs(Math.trunc(pointsCenti));
  return `${Math.floor(abs / 100)}.${(abs % 100).toString().padStart(2, "0")}`;
}

/// The derived prize line for one segment — `5 min`, `2.00 pts`,
/// `10 min + 2.00 pts`, `5 min + spin`, `Free spin`, or `—` for a segment that
/// awards nothing at all.
export function formatPrizeLine(segment: WheelPrizeSegment): string {
  const parts: string[] = [];
  if (segment.seconds > 0) parts.push(formatPrizeDuration(segment.seconds));
  if (segment.points_centi > 0)
    parts.push(`${formatPrizePoints(segment.points_centi)} pts`);

  if (parts.length === 0) return segment.free_spin ? "Free spin" : "—";
  // A prize that also respins says so; "Free spin" alone would hide the prize.
  if (segment.free_spin) parts.push("spin");
  return parts.join(" + ");
}
