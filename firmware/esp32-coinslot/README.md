# ESP32 coin acceptor bridge

This folder is a non-production Milestone 3 firmware placeholder. It shows the
intended flow from a pulse-output coin acceptor to the OpenPiso admin server.

Before local testing:

1. Install ESP32 board support in the Arduino IDE.
2. Copy the Wi-Fi, server, device, and PC values into a local-only version of
   `openpiso-coinslot.ino`.
3. Insert a matching active device ID and secret into the admin SQLite
   `coin_devices` table. OpenPiso intentionally seeds only `dev-simulator`.
4. Confirm the admin PC firewall permits private-LAN TCP traffic to port 3210.
5. Verify the selected GPIO and voltage levels against the hardware notes in
   `docs/hardware.md`.

The current `signature` field carries a temporary shared secret. It is useful
only for basic LAN validation. HMAC-SHA256 signing, replay protection,
provisioning, NVS configuration, calibration, and offline retry storage remain
TODOs before production hardware use.
