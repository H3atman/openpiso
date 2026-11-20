# Install the Client and approve your PCs

Every rental PC runs the Client. The Client locks the PC until a customer has time, and counts that time down. Do these steps on each PC, one at a time.

Before you start, the Central Terminal must be running on the Server PC, and the PC must be on the same network.

## 1. Install the Client

1. Copy the Client installer to the PC. Its file name starts with `OpenPiso.Client`.
2. Double-click the installer.
3. Windows asks for administrator permission. Choose **Yes**. The Client installs for every Windows account on the PC, so it needs this permission.
4. Follow the installer.
5. If the Client does not open by itself when the installer finishes, open **OpenPiso Client** from the Start menu.

The Client fills the screen. This is its lock screen.

## 2. Let the Client find the Server PC

On its first start, the Client looks for the Server PC on your network by itself. You do not need to type anything.

1. Watch the lock screen. It shows **Looking for the OpenPiso server…**.
2. If your network has more than one OpenPiso Server PC, the screen shows **Choose this shop's server** and a list. Pick your shop's name.
3. After a few seconds, the screen changes to **Waiting for owner approval**. Under it, the screen shows a short code after **Code:**. This is the PC's Approval Code. Write it down.

![Client lock screen on a new PC: Waiting for owner approval, approve this PC in the Web Admin under Settings → PCs, with its code](/images/client-waiting-for-approval.png)

If the screen stays on **Looking for the OpenPiso server…**, see [Enter the Server PC address by hand](#enter-the-server-pc-address-by-hand) below.

## 3. Approve the PC in the Web Admin

1. Open the Web Admin on your phone or another computer.
2. Open **Settings**, then **PCs**.
3. Find the **Waiting for approval** list at the top. Each entry shows an Approval Code, the PC's Windows computer name and its address.
4. Find the entry whose code matches the code on the PC's screen. Check the code carefully. Only approve a PC you recognise.
5. Click **Approve** on that entry.

![Web Admin Settings → PCs, Waiting for approval card with one PC: its code, computer name, address and the Approve button](/images/settings-pcs-waiting-for-approval.png)

The Web Admin gives the PC the next free number and a name such as **PC 05**. The PC now appears under **Registered PCs**. Within a few seconds, the PC's lock screen changes to **Insert coin**.

Customers pick a PC at the Central Terminal by its number. Put a sticker with that number on the PC, so customers know which number to type.

You should now see the PC in **Registered PCs**, and **Insert coin** on the PC's lock screen.

## Give a PC a different name

1. In **Settings → PCs**, find the PC under **Registered PCs**.
2. Type the new name in its **Display name** box.
3. Click **Save** on that row.

The number customers type at the Central Terminal does not change.

You should now see the new name on the Dashboard and on the PC's lock screen.

## Enter the Server PC address by hand

Use this when the Client cannot find the Server PC by itself. This can happen when the PC is on a different part of the network from the Server PC.

You need the Server PC's address. You wrote it down when you [installed the Central Terminal](/install-central-terminal). It looks like `192.168.1.10`.

1. On the PC's lock screen, click **Enter address manually**.
2. In **Admin server address**, type the Server PC's address, for example `192.168.1.10`.
3. Click **Save and connect**.
4. The screen changes to **Waiting for owner approval**. Approve the PC as in step 3 above.

![Client lock screen after Enter address manually: Enter the server address, the Admin server address box and the Save and connect button](/images/client-enter-address.png)

To go back to the automatic search, click **Search again**.

You should now see **Waiting for owner approval** and an Approval Code on the PC.

## Add a PC before you install the Client

If a PC has a fixed address on your network, you can add it in the Web Admin first. Then the PC needs no approval.

1. In **Settings → PCs**, find **Pre-register a PC**.
2. In **PC id**, type an id such as `pc-05`. The number at the end is the number customers type at the Central Terminal.
3. In **Display name**, type a name such as `Counter 5`.
4. In **Static IP (optional)**, type the PC's fixed address. Do not leave it empty: the PC uses this address to find its entry.
5. Click **Pre-register**.

The next time the Client on that PC starts, it joins this entry by itself.

You should now see the PC under **Registered PCs**. Its **Bound MAC** fills in after the PC's first start.

Next: [Set your Shop Rate and Shop Hours](/shop-rate-and-hours)
