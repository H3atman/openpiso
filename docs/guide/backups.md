# Backups

A Backup is a copy of all your shop's records: PCs, accounts, sales, settings and PINs. If the Server PC breaks, or something goes wrong after an update, you can put a Backup back.

You manage Backups in the Web Admin. Click **Backups** in the sidebar.

## Make a Backup now

1. Open **Backups**.
2. Under **Create a Backup**, leave all seven boxes ticked. A Backup can only be put back if all seven are ticked.
3. Click **Create Backup**.

The new Backup appears at the top of the **Backup Catalog**, with **manual** under **Kind**.

You should now see the new Backup in the **Backup Catalog**, with today's date under **Created**.

## Keep a copy away from the Server PC

Backups are saved on the Server PC. If the Server PC's disk fails, the Backups on it are lost too. Keep a copy somewhere else.

1. In the **Backup Catalog**, click **Download** next to a Backup.
2. Save the file on your phone or computer, or copy it to a USB flash drive.

![Web Admin Backups page, Backup Catalog with scheduled and manual Backups and their Download, Restore and Delete actions](/images/backup-catalog.png)

A full Backup contains your PINs (in scrambled form) and the secret keys of your devices. Keep Backup files private. Do not share them or post them online.

You should now have a Backup file saved outside the Server PC.

## Make Backups automatically

1. Open **Backups**.
2. Under **Backup Schedule**, turn on **Enable automatic Backups**.
3. In **Frequency**, choose **Daily**, **Weekly** or **Monthly**. For Weekly, choose the **Day of week**. For Monthly, choose the **Day of month**.
4. In **Time of day**, choose a time when the Server PC is usually on. If the Server PC was off at that time, it makes the Backup when it next starts.
5. In **Backups to retain**, type how many automatic Backups to keep. Older automatic Backups are deleted. Backups you make yourself are never deleted automatically.
6. Optional: in **Mirror Directory**, type a folder on another disk, for example `D:\Backups`. Each new Backup is also copied there.
7. Under **Backup Sets for automatic Backups**, leave all seven boxes ticked.
8. Click **Save Backup Schedule**.

![Web Admin Backups page, Backup Schedule card with automatic Backups on, Daily at 3:00 AM, and the Next automatic Backup time](/images/backup-schedule.png)

You should now see the date and time under **Next automatic Backup**.

## Backups made before an update

Some updates change how the Central Terminal stores your records. Before such an update changes anything, the Central Terminal always makes a Backup by itself. It shows as **pre_migration** under **Kind**. You need it if you ever have to go back to the version before the update.

## Put a Backup back

Putting a Backup back replaces all your current records with the records in the Backup. Anything that happened after the Backup was made is lost, including sales.

Before you start, make sure no PC is in use and no PC has Remaining Time. The Web Admin refuses to put a Backup back until then, and lists the PCs that are in the way.

1. Open **Backups**.
2. Choose the Backup:
   - For a Backup in the **Backup Catalog**, click **Restore** next to it.
   - For a Backup file on this phone or computer, use **Restore from a browser upload**: choose the file, then click **Review Upload Restore**.
3. Read the message, then click **Confirm Restore**.
4. The Web Admin checks the Backup. Then a **Pending Restore** box appears at the top of the page.
5. Restart the Server PC. If the Central Terminal does not start by itself, open **OpenPiso Central Terminal** from the Start menu.

When the Central Terminal starts, it first saves your current records as one more Backup, with **pre_restore** under **Kind**. Then it puts your chosen Backup in place.

To change your mind before you restart, click **Cancel Pending Restore**.

You should now see the records from the Backup in the Web Admin.

Next: [Updating](/updating)
