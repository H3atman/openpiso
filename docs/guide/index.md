---
layout: home

hero:
  name: OpenPiso
  tagline: Coin-operated PC rental control for Pisonet shops that runs entirely on your shop's own network.
  image:
    light: /openpiso-symbol-color.svg
    dark: /openpiso-symbol-reversed.svg
    alt: OpenPiso
  actions:
    - theme: brand
      text: Read the setup guide
      link: /what-you-need
    - theme: alt
      text: Download
      link: https://github.com/H3atman/openpiso/releases/latest

features:
  - title: Works on your own network
    details: Your PCs and the Server PC talk over your shop's local network. You do not need internet to run the shop.
  - title: Coins become time automatically
    details: A customer picks a PC at the Central Terminal and drops coins in the coin slot. The time goes straight to that PC.
  - title: Sales reports
    details: See what each day brought in from the Web Admin, on your phone or any computer in the shop.
  - title: Free to use for your shop
    details: You can run OpenPiso in your own Pisonet shop at no cost.
---

![Web Admin dashboard for Demo Pisonet: today's sales, PCs In Use, the PC list with In Use, Online and Offline PCs, and Recent Transactions](/images/dashboard.png)

## What you need

- A Windows computer to be your Server PC. It needs a COM header on the motherboard.
- A coin slot, powered from the Server PC.
- Your rental PCs, running Windows.
- A router or switch that connects them all.

See [What you need](/what-you-need) for the full list.

## Which file do I download?

Open the [latest release page](https://github.com/H3atman/openpiso/releases/latest). It has two installers.

- The file whose name contains **Central Terminal** goes on the Server PC. Install it once.
- The file whose name contains **Client** goes on every rental PC.
