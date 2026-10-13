// @vitest-environment jsdom
import { createRawSnippet, type Component, type Snippet } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, type RenderResult } from "@testing-library/svelte";
import { createMatchMedia } from "../../test/match-media";
import ResponsiveRows from "./ResponsiveRows.svelte";

type Row = {
  id: string;
  label: string;
};

const rows: Row[] = [
  { id: "first", label: "First record" },
  { id: "second", label: "Second record" },
];

type ResponsiveRowsProps = {
  rows: readonly Row[];
  row: Snippet<[Row]>;
  card: Snippet<[Row]>;
};

const ResponsiveRowsForTest =
  ResponsiveRows as unknown as Component<ResponsiveRowsProps>;

function setMatchMedia(matches: boolean): void {
  vi.stubGlobal("matchMedia", createMatchMedia(matches));
}

function renderRows(
  matches: boolean,
): RenderResult<typeof ResponsiveRowsForTest> {
  setMatchMedia(matches);

  return render(ResponsiveRowsForTest, {
    props: {
      rows,
      row: createRawSnippet<[Row]>((getRow) => ({
        render: () =>
          `<div data-testid="row" data-record-id="${getRow().id}">${getRow().label}</div>`,
      })),
      card: createRawSnippet<[Row]>((getRow) => ({
        render: () =>
          `<div data-testid="card" data-record-id="${getRow().id}">${getRow().label}</div>`,
      })),
    },
  });
}

function recordIds(
  view: RenderResult<typeof ResponsiveRowsForTest>,
  testId: string,
): string[] {
  return view
    .getAllByTestId(testId)
    .map((element) => element.getAttribute("data-record-id") ?? "");
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ResponsiveRows", () => {
  it("renders only the row variant when the mobile query does not match", () => {
    const view = renderRows(false);

    expect(recordIds(view, "row")).toEqual(["first", "second"]);
    expect(view.queryByTestId("card")).toBeNull();
  });

  it("renders only the card variant when the mobile query matches", () => {
    const view = renderRows(true);

    expect(recordIds(view, "card")).toEqual(["first", "second"]);
    expect(view.queryByTestId("row")).toBeNull();
  });

  it("iterates the same records in both variants", () => {
    const desktop = renderRows(false);
    const desktopIds = recordIds(desktop, "row");
    desktop.unmount();

    const mobile = renderRows(true);

    expect(recordIds(mobile, "card")).toEqual(desktopIds);
  });
});
