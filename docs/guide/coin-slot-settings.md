# Connect and test the coin slot

On this page you tell OpenPiso where the coin slot is connected, check that it can talk to it, and check that a ₱20 coin counts as ₱20. Wire the coin slot first: see [Wire and set up the coin slot](/coin-slot-wiring).

## How a customer buys time

A customer buys time at the Central Terminal on the Server PC. They type the number of the PC they want and press Enter. Only then does the coin slot take coins; at any other time it returns them. The customer inserts their coins, then presses Enter again to finish. If no coin goes in for 15 seconds, the Central Terminal finishes on its own. OpenPiso counts one peso for every pulse the coin slot sends, which is why the coin slot must send one pulse per peso.

## Open the Coin Device settings

1. Open the Web Admin and sign in.
2. Go to **Settings**, then **Coin Device**.

## Choose the port and the pins

1. Under **COM Port**, press **Refresh**. Then choose the Server PC's COM port, usually **COM1**. A port on the motherboard's COM header is usually described as **Onboard serial** or **Serial port**.
2. Do not choose a port described with a USB product or brand name. That is a USB-to-serial adapter, which is not supported.
3. Do not choose **Simulator (no port)**. It means no coin slot is connected.
4. Leave **Baud Rate** as it is.
5. Set **Set Pin (control)** to **RTS**. This is the green wire on pin 7.
6. Set **Coin Pin (status)** to **DSR**. This is the blue wire on pin 6.
7. Leave **Coin Pin Polarity** on **Normally open (pulse high)**. This is the setting tested with the wiring in this guide, even though the hint under it mentions Normally closed.
8. Leave the **Timing** values as they are. **Inactivity Timeout** is how long the Central Terminal waits for the next coin (15 seconds). **Hard Cap** is the longest a customer can take to insert coins (180 seconds).
9. Press **Save Coin Device**.

## Test the connection

1. Press **Test Acceptor**.
2. Read the **Acceptor test result** box. If OpenPiso can use the port, it says the port is open and the source is available, for example: "COM1 is open using set=rts and coin=dsr; the source is available."
3. If it says the port "is configured but the serial source is unavailable", check that you chose the right port and that no other program is using it.

**Test Acceptor** only checks that OpenPiso can open the port. It does not count a coin. The ₱20 Calibration below does that.

## Run the ₱20 Calibration

The ₱20 Calibration checks how many pulses your coin slot really sends for a ₱20 coin. On a new install, the page shows a notice until you finish it. Coins you insert during the calibration are not added to any PC.

Have five real ₱20 coins ready.

1. Find the **Coin Calibration** section. Leave **Target amount (cents)** at **2000**, which is ₱20.
2. Press **Start ₱20 Calibration**. The serial settings above are locked until you finish or cancel.
3. Insert exactly one ₱20 coin. Wait until **Pulses captured** stops going up. It should stop at **20**.
4. Press **Record Sample**.
5. Press **Arm Next Sample**. The coin slot returns coins until you press it.
6. Repeat steps 3 to 5 until five samples are recorded.
7. Check the **Samples** line. All five should read 20.
8. Press **Apply Mapping**. You can only apply when all five samples agree. If they do not agree, press **Discard** and start again.

If a sample is not 20, the coin slot is not sending one pulse per peso for ₱20. Teach the ₱20 coin again (see [Wire and set up the coin slot](/coin-slot-wiring)), then start the calibration again.

## Read Acceptor Health

The **Acceptor Health** section shows when the current coin record started, under **Coin Debug Window start**. Each time you press **Save Coin Device**, a new one starts. Press **Refresh** to update it.

In this version, **Inter-pulse timing** and **Implausible Timing rate** are not measured yet. The section shows them as not available. This is normal.

![Web Admin Settings → Coin Device, Acceptor Health card with Coin Debug Window start, Inter-pulse timing and Implausible Timing rate](/images/acceptor-health.png)

You should now see a successful **Acceptor test result** and a ₱20 Calibration of five samples that all read 20. You test a real purchase at the Central Terminal after you add your PCs.

## Coins not counting or counting wrong {#coins-not-counting}

Check these one at a time. Switch off and unplug the Server PC before you change any wire.

- **NO / NC switch on NO.** In any other position, coins are missed or counted wrongly.
- **Speed switch on Slow.** Fast or Medium pulses are too short to count reliably.
- **Pull-up wire on pin 4.** One branch of the blue COIN wire must go to pin 4 (DTR). Without it, coins are not counted, or time is added when no coin went in.
- **One pulse per peso.** Each coin must be taught to send one pulse per peso: ₱1 sends 1, ₱5 sends 5, ₱10 sends 10, ₱20 sends 20. Run the ₱20 Calibration to check.
- **The COM header on the motherboard.** The coin slot must be wired to the motherboard's COM header, not to a USB-to-serial adapter.
- **The right port.** Under **COM Port**, the chosen port must be the motherboard's COM port, with **Set Pin (control)** on **RTS** and **Coin Pin (status)** on **DSR**. Press **Test Acceptor** to check it.

Next: [Install the Client and approve your PCs](/install-client)
