# Set your Shop Rate and Shop Hours

You set your shop's name, price and opening hours in the Web Admin, under **Settings**, then **Shop**.

## Set the Shop Name

The Shop Name shows on the Web Admin dashboard and on every PC's lock screen.

1. Open **Settings**, then **Shop**.
2. Under **Shop Details**, type your shop's name in **Shop name**. Use up to 80 characters.
3. Leave the two other fields as they are, unless you have a reason to change them.
4. Click **Save Shop Details**.

You should now see your shop's name on the lock screen of each PC.

## Set the Shop Rate

The Shop Rate is how much time a customer gets for their money. A new install starts at ₱1 for 5 minutes (300 seconds).

1. Under **Shop Rate**, type the price in **Amount (whole pesos)**, for example `5`.
2. Type the time that price buys in **Seconds**, for example `1800` for 30 minutes. The box shows the time in minutes and hours under it.
3. Check **Configured coin purchase preview**. It shows how much time each coin buys, for example **₱5 buys 30 minutes**.
4. Click **Save Shop Rate**.

![Web Admin Settings → Shop, Shop Rate card set to ₱1 for 240 seconds, with the coin purchase preview](/images/settings-shop-rate.png)

A coin's time is always rounded down to a whole second. If a coin loses part of a second, the preview shows a note under that coin. You can still save.

A new Shop Rate applies to coins inserted after you save. Time that customers already bought does not change.

You should now see the new time per coin in **Configured coin purchase preview**.

## Set your Shop Hours

Shop Hours tell OpenPiso when your shop is open. When the shop is closed, customers cannot add time or sign in. A customer who is already using a PC keeps their time and can keep playing until it runs out.

Shop Hours are off on a new install, so the shop is always open.

1. Under **Shop Hours**, turn on **Enable Shop Hours**.
2. For each day, choose one of these in its box:
   - **Open Period**: open between two times. Type the start and end times. An Open Period can end after midnight, for example from 18:00 to 02:00.
   - **Closed all day**.
   - **Open 24 hours**.
3. To give every day the same hours, set one day, then click **Copy to all days** on that day.
4. In **Last Call lead minutes**, type how many minutes before closing the warning starts. During Last Call, customers can still buy time, but they see that the shop closes soon. Type `0` to turn Last Call off.
5. Click **Save Shop Hours**.

![Web Admin Settings → Shop, Shop Hours card with Enable Shop Hours on, Shop Status Shop Open, and Open Periods for every day of the week](/images/settings-shop-hours.png)

The box at the top shows the **Shop Status**: **Shop Open** or **Shop Closed**, and when it next changes.

You should now see **Shop Open** or **Shop Closed** under Shop Status, matching your hours.

## Open or close the shop early

A Shop Override opens or closes the shop until a time you choose, whatever your Shop Hours say. Use it for a holiday, or to stay open late one night.

1. Under **Shop Override**, choose **Force-closed** or **Force-open** in **Mode**.
2. In **Expires at**, choose when the override ends. It starts with your next scheduled change.
3. Click **Set Shop Override**.

When the override ends, your normal Shop Hours apply again. To end it early, click **Clear Shop Override**.

You should now see, for example, **Force-closed until** and the end time, at the top of the Shop Override card.

Next: [Running the shop day to day](/daily-operation)
