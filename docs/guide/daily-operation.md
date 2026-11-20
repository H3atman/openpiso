# Running the shop day to day

This page covers what you do while the shop is open: watch your PCs, help a customer buy time, fix a PC's time by hand, and check your sales.

## Watch your PCs on the Dashboard

The Dashboard is the first page you see after you sign in to the Web Admin. It updates by itself every few seconds.

1. Open the Web Admin and sign in with your Web Admin PIN.
2. Click **Dashboard** in the sidebar if you are on another page.

![Web Admin dashboard for Demo Pisonet: today's sales, PCs In Use, the PC list with In Use, Online and Offline PCs, and Recent Transactions](/images/dashboard.png)

At the top you see three numbers:

- **Today's Sales**: the money taken today, and how many purchases that was.
- **Time Sold Today**: the total time customers bought today.
- **PCs In Use**: how many PCs are in use, out of all your PCs.

The **PCs** list shows each PC with:

- Its name, and who is using it: an account name, or **Guest** for a customer who did not sign in.
- Its status: **In Use**, **Online** (on and free), **Offline** (off, or cannot reach the Server PC) or **Locked**.
- Its **Remaining Time**.

**Recent Transactions** lists the latest purchases: the PC, the customer, the time added and the amount paid.

You should now see every PC you approved, with its status and Remaining Time.

## Help a customer buy time

Customers pay at the Central Terminal, on the Server PC. The coin slot is there, not at the PCs.

1. The customer types their PC's number on the Central Terminal keypad. The number is on the PC's sticker. The Central Terminal also lists every PC under **All PCs**.
2. The Central Terminal shows that PC. The customer presses **Enter** to open it.
3. The screen shows **Insert Coin**. The customer drops coins into the coin slot. Each coin's time shows under **Added**.
4. When they are done, the customer presses **Enter** (**Done**). If they stop inserting coins, the screen closes by itself after a few seconds.

![Central Terminal insert screen for PC 07 after a ₱5 coin: seconds left to insert another coin, Inserted ₱5.00, Added +00:20:00, and the Release (Esc) and Done (Enter) buttons](/images/central-terminal-insert-coin.png)

Every coin counts as soon as it drops. Closing the screen never loses a coin.

If a customer is signed in to an account on that PC, the time goes to their account. If not, it goes to the PC as Guest Time.

You should now see the new time on the PC's screen, and the purchase under **Recent Transactions** on the Dashboard.

## Add or remove Guest Time by hand

Use this to give a customer time back, for example after a power cut. It changes the Guest Time on one PC.

1. Click **Adjustments** in the sidebar.
2. Under **Adjust Guest Time**, choose the PC in **PC**. The list shows each PC's remaining time.
3. In **Seconds Delta**, type the number of seconds to add, for example `600` for 10 minutes. To remove time, type a minus number, for example `-600`. Time never goes below zero.
4. In **Reason (optional)**, type why, for example `Power cut`.
5. Click **Apply Adjustment**.

![Web Admin Adjustments page: Adjust Guest Time card with PC 03 chosen, 600 in Seconds Delta and a reason typed](/images/adjustments.png)

A manual adjustment is not a sale. It does not show in your sales.

You should now see the PC's new remaining time in the **PC** list and on the Dashboard.

## Check your sales

1. Click **Sales Report** in the sidebar.
2. At the top, read **Daily Sales**, **Weekly Sales**, **Monthly Sales** and **Total Sales**. Each one shows the dates it covers.
3. Under **Daily Sales Trend**, choose a month and a year to see each day's sales on a chart.
4. Under **PC Breakdown**, see how much each PC brought in.

![Web Admin Sales Report: Daily, Weekly, Monthly and Total Sales, the Daily Sales Trend chart and the PC Breakdown table](/images/sales-report.png)

You should now see your sales for today, this week, this month and in total.

## Open a PC's settings with the Owner PIN

Each PC's lock screen has a settings button that only you can use. It is the gear button in the bottom corner of the lock screen.

1. On the PC's lock screen, click the gear button.
2. Type your Owner PIN in **Owner PIN**.
3. Click **Unlock**.

The **PC settings** panel opens. From here you can:

- Change the Server PC's address in **Admin server address**.
- Choose whether the Client starts when Windows starts, with **Launch at startup**.
- Close the Client on this PC, with **Quit client…**. Use this for maintenance only.

Close the panel with the **✕** button when you are done.

You should now see the **PC settings** panel, with the PC's id under **PC id**.

Next: [Backups](/backups)
