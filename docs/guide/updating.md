# Updating

The Central Terminal updates itself from the Web Admin when you tell it to. The Client on each PC you update by hand.

## Update the Central Terminal

The Server PC needs internet to find and download an update. The Central Terminal checks for updates when it starts and every 6 hours.

1. Open the Web Admin.
2. In the sidebar, find the version row, just above the owner menu at the bottom. On a phone, open the sidebar first. The row shows **App Update** and your version, or **Update available** and the new version.
3. Click the row. The **App Update** panel opens.
4. To check right now, click **Check now**.
5. If an update is available, the panel shows its version and what is new in it. Click **Install**.
6. The panel asks **Download v…?** If it also shows a red warning that the update raises the Schema Version, read it and tick **I understand**. See [Updates that change the database](#updates-that-change-the-database) below.
7. Click **Download update**. The Central Terminal downloads and checks the update while the shop keeps running.
8. When the panel says the update is downloaded and verified, pick a quiet moment, then click **Restart and install**.

The Central Terminal closes, installs the update and opens again by itself. This takes a short time. Nothing restarts until you click **Restart and install**.

If PCs are in use, the panel shows how many. You can still update. During the restart, those PCs show their lock screen with a countdown. They come back when the Central Terminal is running again. No one loses their time.

When the Central Terminal is back, sign in to the Web Admin again. A message shows **Updated to v…** or **Update failed, still on v…**.

You should now see the new version in the sidebar version row.

## Updates that change the database

Some updates change how the Central Terminal stores your records. The panel marks these with a red warning, and you must tick **I understand** before you can download them.

Before such an update changes anything, the Central Terminal makes a Backup by itself. After the update, you cannot go back to an older version from the App Update panel. The only way back is to put that Backup back, which loses everything that happened since the update. See [Backups](/backups).

## Go back to an older version

If an update causes a problem, you can go back to an older version, as long as it stores records the same way.

1. Open the **App Update** panel.
2. Click **Compatible Rollback**. It lists the older versions you can go back to. If it says **No Compatible Rollback releases available.**, you cannot go back from here.
3. Click **Roll back to v…** under the version you want.
4. Read the red warning. Type the version number exactly as shown, for example `0.12.10`.
5. Click **Download rollback**.
6. When it is downloaded, click **Restart and roll back**.

You should now see the older version in the sidebar version row.

## Update without internet

If the Server PC has no internet, update it by hand.

1. On another computer, download the new Central Terminal installer from the [Download](/download) page.
2. Copy it to the Server PC.
3. Sign in to Windows on the Server PC with the same account the Central Terminal runs under.
4. Run the installer.

Never install an older Central Terminal version over a newer one. If you need to go back, use [Compatible Rollback](#go-back-to-an-older-version) or a [Backup](/backups).

You should now see the new version in the sidebar version row.

## Update the Client on each PC

The Client does not update itself, and the Central Terminal does not update it. Update it on each PC by hand. Do this when no one is using the PC.

1. Download the new Client installer from the [Download](/download) page. Its file name starts with `OpenPiso.Client`.
2. Copy it to the PC.
3. Double-click the installer. Windows asks for administrator permission. Choose **Yes**.
4. Follow the installer. It replaces the old Client.
5. Restart the PC.

The PC keeps its number, its name and its time. You do not need to approve it again.

You should now see the new version at the bottom of the PC's lock screen, after **Client**.

Next: [Troubleshooting](/troubleshooting)
