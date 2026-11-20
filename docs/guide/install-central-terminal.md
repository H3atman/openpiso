# Install the Central Terminal

The Central Terminal runs on the Server PC. It keeps the shop's records, counts the coins and shows customers the screen where they pick a PC. You manage it from the Web Admin, in a web browser on your phone or another computer.

You need the Central Terminal installer from the [Download](/download) page.

## What OpenPiso sends over the internet

Once a day, the Central Terminal sends one anonymous summary to the OpenPiso project, through PostHog, a service in the United States. The summary has a random ID made for your installation, the app version, the date, and how many PCs were active in the last day and the last 30 days. It never includes customer, money, time or shop-identifying information. PostHog sees your internet address when the summary arrives but is set to discard it. Summaries are kept for one year, and the summaries from one shop cannot be deleted. You cannot turn this off.

## Steps

### 1. Write down the Server PC's address

Do this before you install. Once the Central Terminal starts, it covers the whole screen.

1. On the Server PC, open Windows **Settings**.
2. Go to **Network & internet**.
3. Open your connection, for example **Ethernet**.
4. Find **IPv4 address**. It looks like `192.168.1.10`. Write it down.

This is the Server PC's address. You use it to open the Web Admin.

### 2. Run the installer

1. On the Server PC, double-click the file that starts with `OpenPiso.Central.Terminal`.
2. Follow the installer.

The installer installs the Central Terminal for your Windows user only. It does not ask for administrator rights to install.

### 3. Allow the firewall change

Near the end of the first install, Windows asks once for permission to change the firewall. This lets your rental PCs find and reach the Server PC.

- The prompt names **Windows Command Processor**, from Microsoft Windows, not OpenPiso. This is expected.
- Choose **Yes**.

If you choose **No**, the install still finishes, but your rental PCs may not find the Server PC by themselves. Updates do not ask again once the change is made.

### 4. Look at the Central Terminal

When the Central Terminal starts, it fills the whole screen and stays in front of every other window. This is the screen your customers use.

- At the top it shows the shop name, **OpenPiso Shop**, until you change it.
- It shows **No PC selected** in the middle.
- It has no PCs yet. They appear after you install the Client and approve each PC.

![Central Terminal on a fresh install: OpenPiso Shop header with Kiosk · Locked, No PC selected, and no PCs yet](/images/central-terminal-first-launch.png)

Leave the Central Terminal running. It must stay on while the shop is open.

### 5. Open the Web Admin

1. Connect your phone or another computer to the same network as the Server PC.
2. Open a web browser.
3. Type `http://` then the Server PC's address, then `:8080/admin`. For example: `http://192.168.1.10:8080/admin`.

If the page does not open, check that the phone or computer is on the same network, and that you typed the address exactly.

### 6. Create your Web Admin PIN

The first time you open the Web Admin, it asks you to create your Web Admin PIN. It shows **First-time setup: create your Web Admin PIN to get started.**

1. Type a PIN in **New PIN**. Use 4 to 128 characters.
2. Type the same PIN in **Confirm PIN**.
3. Click **Set PIN**.

![Web Admin first-time setup screen with the create your Web Admin PIN message, New PIN and Confirm PIN boxes and the Set PIN button](/images/web-admin-create-pin.png)

The Web Admin PIN signs you in to the Web Admin. You also use it on the Server PC: double-click the shop name at the top of the Central Terminal and enter this PIN to let other windows come in front of it. Double-click the shop name again to put the Central Terminal back in front. Keep this PIN private.

### 7. Set your Owner PIN

The Owner PIN protects the settings and the **Quit** button of the Client on each rental PC. When you create your Web Admin PIN, the Owner PIN starts out the same. You can give it a different value, so staff who look after the PCs do not need your Web Admin PIN.

1. In the Web Admin, open **Settings**, then **Security**.
2. Find **Owner PIN (Client)**.
3. Type the new PIN in **New PIN** and again in **Confirm new PIN**.
4. Click **Set owner PIN**.

![Web Admin Settings → Security, the Owner PIN (Client) and Web Admin PIN (this console) cards](/images/settings-security.png)

To change your Web Admin PIN later, use **Web Admin PIN (this console)** on the same page. It asks for your current PIN first.

## Keep the Web Admin on your own network

Anyone who can open the Web Admin and knows the PIN can change money, time and accounts. Only open the Web Admin from your own shop network.

- If your shop has Wi-Fi for customers, keep it on a separate network that cannot reach the Web Admin.
- Do not set up your router to open the Web Admin to the internet.

You should now see the Web Admin dashboard on your phone or computer, and the Central Terminal on the Server PC.

![Web Admin dashboard right after the first sign-in, with ₱0.00 sales and no PCs yet](/images/dashboard-empty.png)

Next: [Wire and set up the coin slot](/coin-slot-wiring)
