import { getRedemptionTiers } from "./api";
import type { RedemptionTier, RedemptionTierMenu } from "./types";

/**
 * Tier menu points are whole points on the wire; balances are integer
 * centipoints (ADR-0038). This is the only place that multiplies by 100.
 */
export const CENTIPOINTS_PER_POINT = 100;

export function tierCostCenti(tier: RedemptionTier): number {
  return tier.points * CENTIPOINTS_PER_POINT;
}

export function isTierAffordable(tier: RedemptionTier, balanceCenti: number): boolean {
  return tierCostCenti(tier) <= balanceCenti;
}

/**
 * Centipoints still needed to unlock the cheapest tier, or 0 when at least
 * one tier is affordable (or the menu is empty). Assumes the menu is sorted
 * by points ascending, as `loadSortedTierMenu` guarantees.
 */
export function cheapestTierShortfallCenti(
  menu: RedemptionTierMenu | null,
  balanceCenti: number,
): number {
  if (!menu?.tiers.length) return 0;
  if (menu.tiers.some((tier) => isTierAffordable(tier, balanceCenti))) return 0;
  return Math.max(0, tierCostCenti(menu.tiers[0]) - balanceCenti);
}

export async function loadSortedTierMenu(): Promise<RedemptionTierMenu> {
  const menu = await getRedemptionTiers();
  return { ...menu, tiers: [...menu.tiers].sort((a, b) => a.points - b.points) };
}

/**
 * The server rejects a redeem against an out-of-date menu version with a
 * Conflict whose message mentions "stale"; callers refresh the menu and ask
 * the customer to choose again.
 */
export function isStaleMenuError(caught: unknown): boolean {
  const message = caught instanceof Error ? caught.message : String(caught);
  return message.toLowerCase().includes("stale");
}
