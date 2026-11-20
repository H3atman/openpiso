# Troubleshooting

Find the problem you see, then work through the checks in order.

## Coins do not count, or count the wrong amount

The coin slot's wiring or its settings in the Web Admin may be wrong. See [Coins not counting](/coin-slot-settings#coins-not-counting) on the coin slot page.

## A PC stays on "Waiting for owner approval"

1. Open the Web Admin and go to **Settings**, then **PCs**.
2. Look for the **Waiting for approval** list at the top. Find the entry with the same code as the PC's screen, and click **Approve**. See [Approve the PC in the Web Admin](/install-client#_3-approve-the-pc-in-the-web-admin).
3. If the list is not there, wait a few seconds. The list updates by itself while the PC keeps trying.
4. If the PC still does not appear, check that the Client is still running on the PC and that the PC is connected to the network.
5. After you approve, wait a few seconds. The PC's screen changes to **Insert coin**.

## A PC cannot find the Server PC

The PC's screen stays on **Looking for the OpenPiso server…**, or shows **Can't reach server**.

1. Check that the Central Terminal is running on the Server PC.
2. Check that the PC and the Server PC are connected to the same router or switch.
3. Check that the PC's network cable is plugged in, and that the PC can see other computers on the network.
4. Check the firewall change on the Server PC. When you first installed the Central Terminal, Windows asked for permission for **Windows Command Processor**. If you chose **No**, run the Central Terminal installer again on the Server PC and choose **Yes** this time. Your records stay as they are.
5. If the PC is on a different part of the network from the Server PC, it cannot find the Server PC by itself. Type the address by hand. See [Enter the Server PC address by hand](/install-client#enter-the-server-pc-address-by-hand).
6. If the Server PC's address changed, a PC that connected before does not follow it. Open the PC's settings with your Owner PIN, and type the new address in **Admin server address**. See [Open a PC's settings with the Owner PIN](/daily-operation#open-a-pc-s-settings-with-the-owner-pin). To stop the address changing, ask whoever set up your router to give the Server PC a fixed address.

## You cannot open the Web Admin from your phone

1. Check that the Central Terminal is running on the Server PC.
2. Check that your phone is on the same network as the Server PC. A separate customer or guest Wi-Fi network cannot reach the Web Admin. Turn off mobile data if your phone keeps switching to it.
3. Check the address. It is `http://`, then the Server PC's address, then `:8080/admin`. For example: `http://192.168.1.10:8080/admin`. It starts with `http://`, not `https://`.
4. Check that the Server PC's address has not changed. On the Server PC, look up its address again, as in [Install the Central Terminal](/install-central-terminal).
5. Try the same address from a computer on the same network. If that works, the problem is on the phone.
6. If nothing on the network can open it, Windows Firewall on the Server PC may be blocking the Web Admin. The installer does not open it for you. On the Server PC:
   1. Open the Start menu, type **Windows Defender Firewall with Advanced Security**, and open it.
   2. Click **Inbound Rules**, then **New Rule**.
   3. Choose **Port**, then **TCP**, and type `8080` under **Specific local ports**.
   4. Choose **Allow the connection**.
   5. Give the rule a name, such as **OpenPiso Web Admin**, and finish.

   Only allow this on your own shop network. If customers use a Wi-Fi network that can reach the Server PC, they can reach the Web Admin login too.

## You forgot your Web Admin PIN

The Web Admin has no reset button. You cannot recover the old PIN. You can remove it, and then create a new PIN as on the first day.

This needs a few typed commands on the Server PC. If you are not comfortable doing this, ask someone who is used to the Windows command prompt to do it for you.

If the Web Admin says **Too many attempts**, you typed the wrong PIN too many times. Wait the number of seconds shown, then try again. Restarting the Server PC also clears this wait.

To remove the PIN:

1. Choose a time when no one is using a PC.
2. Download the SQLite command-line tools for Windows from [sqlite.org/download.html](https://www.sqlite.org/download.html). Choose the file named **sqlite-tools-win-x64**. Unzip it, and find `sqlite3.exe` inside.
3. On the Server PC, close the Central Terminal: press Ctrl+Alt+Delete and choose **Task Manager**. Find the Central Terminal in the list. It may show as **OpenPiso Central Terminal** or with your Shop Name. Select it, then click **End task**.
4. Open the Windows Start menu, type `cmd`, and open **Command Prompt**. Use the same Windows account the Central Terminal runs under.
5. Make a copy of your records first. Type this and press Enter:

   ```text
   copy "%APPDATA%\org.openpiso.admin\openpiso.db" "%APPDATA%\org.openpiso.admin\openpiso.db.before-pin-reset"
   ```

6. Remove the PIN. Replace `C:\path\to\sqlite3.exe` with where you unzipped `sqlite3.exe`, then press Enter:

   ```text
   "C:\path\to\sqlite3.exe" "%APPDATA%\org.openpiso.admin\openpiso.db" "DELETE FROM owner_credentials;"
   ```

7. Open **OpenPiso Central Terminal** from the Start menu.
8. Open the Web Admin. It shows **First-time setup: create your Web Admin PIN to get started.** Create a new PIN.

Your new PIN also becomes your Owner PIN for the PCs. All your other records stay as they were.

## An update will not install

1. Check that the Server PC has internet. The Central Terminal needs it to find and download updates.
2. Open the **App Update** panel and read the red message at the top, if there is one. Then click **Check now** again.
3. If the download stopped, click **Install** and **Download update** again. The Central Terminal keeps running on the old version until you click **Restart and install**.
4. If you clicked **Restart and install** and the message after the restart says **Update failed, still on v…**, the shop is still running on the old version. Try again later.
5. If the Server PC has no internet, or the update still fails, update by hand. See [Update without internet](/updating#update-without-internet).
