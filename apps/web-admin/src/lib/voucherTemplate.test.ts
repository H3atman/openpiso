import { describe, expect, test } from "vitest";
import {
  buildVoucherDocument,
  substituteVoucherTokens,
} from "./voucherTemplate";

const allValues = {
  code: "SHOP-ABCD-2345",
  time: "01:00:00",
  points: "1.25",
  price: "₱50.50",
  shop_name: "OpenPiso Shop",
  expires_at: "Jan 2, 2027",
  prefix: "SHOP",
};

describe("substituteVoucherTokens", () => {
  test("substitutes every supported token", () => {
    const template =
      "{{code}}|{{time}}|{{points}}|{{price}}|{{shop_name}}|{{expires_at}}|{{prefix}}";

    expect(substituteVoucherTokens(template, allValues)).toBe(
      "SHOP-ABCD-2345|01:00:00|1.25|₱50.50|OpenPiso Shop|Jan 2, 2027|SHOP",
    );
  });

  test("replaces missing values with an empty string", () => {
    expect(
      substituteVoucherTokens("Code {{code}} / {{expires_at}}", {
        code: "ABC",
      }),
    ).toBe("Code ABC / ");
  });

  test("HTML-escapes substituted values", () => {
    expect(
      substituteVoucherTokens("<strong>{{shop_name}}</strong>", {
        shop_name: `<&>\"'`,
      }),
    ).toBe("<strong>&lt;&amp;&gt;&quot;&#39;</strong>");
  });

  test("leaves unsupported placeholders unchanged", () => {
    expect(substituteVoucherTokens("{{unknown}}", allValues)).toBe(
      "{{unknown}}",
    );
  });
});

describe("buildVoucherDocument", () => {
  test("removes script tags and disables script execution with CSP", () => {
    const document = buildVoucherDocument(
      {
        html: `<article>{{code}}<script>window.top.hacked = true</script></article>`,
        css: `article { color: black; }</style><script>window.top.hacked = true</script>`,
      },
      [allValues],
    );

    expect(document).not.toMatch(/<script/i);
    expect(document).toContain("script-src 'none'");
    expect(document).toContain("SHOP-ABCD-2345");
  });
});
