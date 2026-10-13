import { MediaQuery } from "svelte/reactivity";

const DEFAULT_MOBILE_BREAKPOINT = 768;

/** The one responsive switch point (ADR-0065), as a matchMedia query string. */
export function mobileMediaQuery(
  breakpoint: number = DEFAULT_MOBILE_BREAKPOINT,
): string {
  return `(max-width: ${breakpoint - 1}px)`;
}

export class IsMobile extends MediaQuery {
  constructor(breakpoint: number = DEFAULT_MOBILE_BREAKPOINT) {
    super(mobileMediaQuery(breakpoint));
  }
}
