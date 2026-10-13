import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type WithoutChild<T> = T extends { child?: any } ? Omit<T, "child"> : T;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type WithoutChildren<T> = T extends { children?: any }
  ? Omit<T, "children">
  : T;
export type WithoutChildrenOrChild<T> = WithoutChildren<WithoutChild<T>>;
export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & {
  ref?: U | null;
};

/**
 * Compile-time exhaustiveness guard for a closed union rendered as a markup
 * branch chain. Placed in the trailing `{:else}`, it narrows the discriminant
 * to `never`, so a screen added to the union without a branch fails to compile
 * instead of silently rendering nothing. Returns a string so it can sit in
 * markup position; unreachable at runtime.
 */
export function assertUnreachable(unrendered: never): string {
  return String(unrendered);
}

/**
 * Svelte action: opens the native date picker on any click within the input.
 * Avoids the frustrating "click exactly the tiny calendar icon" UX.
 * Gracefully falls back when showPicker() isn't available.
 */
export function opendatepicker(node: HTMLInputElement) {
  function onClick() {
    try {
      node.showPicker();
    } catch {
      // showPicker unsupported or blocked — native UX still works
    }
  }
  node.addEventListener("click", onClick);
  return {
    destroy() {
      node.removeEventListener("click", onClick);
    },
  };
}

/**
 * Wraps a dynamic `import()` so the module is fetched on first render and the
 * *same* promise is returned afterwards. Await blocks key off promise identity,
 * so a fresh promise per call would restart the block on every reactive update;
 * caching keeps a revisited screen mounting without a loading flash.
 */
export function lazy<T>(load: () => Promise<T>): () => Promise<T> {
  let pending: Promise<T> | undefined;
  return () => (pending ??= load());
}
