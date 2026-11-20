import { defineConfig } from "vitepress";

const releases = "https://github.com/H3atman/openpiso/releases/latest";

// Owner-facing user guide, deployed to GitHub Pages by .github/workflows/guide.yml.
// The sidebar file names are the contract later guide pages fill in.
export default defineConfig({
  title: "OpenPiso",
  description: "Coin-operated PC rental control for Pisonet shops.",
  base: "/openpiso/",
  cleanUrls: true,
  srcExclude: ["reference/**"],
  head: [
    [
      "link",
      { rel: "icon", type: "image/svg+xml", href: "/openpiso/favicon.svg" },
    ],
  ],
  themeConfig: {
    logo: {
      light: "/openpiso-symbol-color.svg",
      dark: "/openpiso-symbol-reversed.svg",
      alt: "OpenPiso",
    },
    search: { provider: "local" },
    nav: [
      { text: "Guide", link: "/what-you-need" },
      { text: "Download", link: releases },
    ],
    socialLinks: [
      { icon: "github", link: "https://github.com/H3atman/openpiso" },
    ],
    sidebar: [
      {
        text: "Set up your shop",
        items: [
          { text: "What you need", link: "/what-you-need" },
          { text: "Download", link: "/download" },
          {
            text: "Install the Central Terminal",
            link: "/install-central-terminal",
          },
          { text: "Wire and set up the coin slot", link: "/coin-slot-wiring" },
          {
            text: "Connect and test the coin slot",
            link: "/coin-slot-settings",
          },
          {
            text: "Install the Client and approve your PCs",
            link: "/install-client",
          },
          {
            text: "Set your Shop Rate and Shop Hours",
            link: "/shop-rate-and-hours",
          },
        ],
      },
      {
        text: "Run your shop",
        items: [
          { text: "Running the shop day to day", link: "/daily-operation" },
          { text: "Backups", link: "/backups" },
          { text: "Updating", link: "/updating" },
          { text: "Troubleshooting", link: "/troubleshooting" },
        ],
      },
    ],
  },
});
