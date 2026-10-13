import { configure } from "@testing-library/svelte";

import { createMatchMedia } from "./match-media";

// Component tests mount lazily imported route screens, so a first paint can
// wait on a module transform rather than a mocked fetch. The one-second
// default turns that into load-dependent flakiness; five seconds still fails
// fast on a genuine hang.
configure({ asyncUtilTimeout: 5_000 });

class TestResizeObserver implements ResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}

  observe(target: Element): void {
    Object.defineProperties(target, {
      clientWidth: { configurable: true, value: 720 },
      clientHeight: { configurable: true, value: 120 },
    });
    const size = { blockSize: 120, inlineSize: 720 };
    this.callback(
      [
        {
          target,
          contentRect: new DOMRect(0, 0, 720, 120),
          borderBoxSize: [size],
          contentBoxSize: [size],
          devicePixelContentBoxSize: [size],
        },
      ],
      this,
    );
  }

  unobserve(): void {}

  disconnect(): void {}
}

if (typeof window !== "undefined") {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: createMatchMedia(),
  });

  Object.defineProperty(globalThis, "ResizeObserver", {
    configurable: true,
    writable: true,
    value: TestResizeObserver,
  });

  Object.defineProperty(Element.prototype, "animate", {
    configurable: true,
    writable: true,
    value: () => {
      const animation = {
        cancel() {},
        currentTime: 0,
        onfinish: null,
        playState: "finished",
      } as unknown as Animation;
      queueMicrotask(() => {
        animation.onfinish?.call(
          animation,
          new Event("finish") as AnimationPlaybackEvent,
        );
      });
      return animation;
    },
  });
}
