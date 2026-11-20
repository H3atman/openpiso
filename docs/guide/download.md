# Download

OpenPiso comes as two installers. You download both from the OpenPiso release page.

## Which file goes where

| Installer        | Its file name starts with   | Install it on       |
| ---------------- | --------------------------- | ------------------- |
| Central Terminal | `OpenPiso.Central.Terminal` | The Server PC, once |
| Client           | `OpenPiso.Client`           | Every rental PC     |

Both file names end in `-setup.exe`. The part in the middle is the version number. It changes with each release.

## Steps

1. Open the [latest release page](https://github.com/H3atman/openpiso/releases/latest) in a web browser.
2. Scroll down to **Assets**.
3. Click the file that starts with `OpenPiso.Central.Terminal` and ends in `-setup.exe`. Your browser saves it.
4. Click the file that starts with `OpenPiso.Client` and ends in `-setup.exe`. Your browser saves it too.
5. Copy each installer to the computer it belongs on. A USB flash drive works. You need the Client installer on every rental PC.

![Assets list on the latest OpenPiso GitHub release, including the OpenPiso.Central.Terminal and OpenPiso.Client setup files](/images/release-assets.png)

The release page also lists other files, such as files ending in `.sig` or `.json`. You do not need them. OpenPiso uses them for updates.

Always download from the latest release page. It has the newest version of both installers.

## When Windows shows a warning

The installers are not signed with a Windows code-signing certificate. Because of this, Windows SmartScreen may show a warning before it runs a downloaded installer. This is expected for OpenPiso installers that you got from the release page above.

Only run installers that you downloaded from the OpenPiso release page yourself.

You should now see both installers saved, ready to copy to the Server PC and your rental PCs.

Next: [Install the Central Terminal](/install-central-terminal)
