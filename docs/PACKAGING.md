# Packaging, updates and rollback

CodexBar ships as an MSIX package (#93). `package.ps1` builds the release
executable, packs it with [`packaging/AppxManifest.xml`](../packaging/AppxManifest.xml),
signs it and publishes it to a **channel folder** together with an App Installer
file (`CodexBar.appinstaller`). Windows installs from that file and checks it for
updates every time CodexBar starts.

The package also registers CodexBar's Windows widget provider (#94); see
[Widgets](#widgets).

## Signing

Packages are signed with a **self-signed certificate**,
`CN=HemSoft CodexBar Self-Signed`. `package.ps1` creates it in your certificate
store (`Cert:\CurrentUser\My`) on first use. Its private key can't be exported,
so nothing secret is written to disk or to the channel. The channel gets only
the public certificate (`CodexBar.cer`).

Windows installs a self-signed package only when its certificate is trusted in
the local machine's **Trusted People** store. `.\package.ps1 -Trust` adds it
there. That needs administrator approval once per PC (Windows shows a prompt).
This trust applies to all users of the PC; remove it when you no longer need it
(see [Uninstall](#uninstall)).

The certificate is valid for five years. `package.ps1` keeps using it, and warns
when fewer than 30 days remain. It never replaces the certificate on its own:
PCs that trust only the old certificate would refuse every update signed with a
new one. Renew it with `-RenewCertificate`, then trust the new `CodexBar.cer` on
every PC that installs CodexBar.

A public release later needs a certificate that every PC already trusts. The
options are the Microsoft Store (free, and it signs the package for you), Azure
Artifact Signing, or an OV code-signing certificate. That decision is open and
belongs to the maintainer. When it is made, the manifest's `Publisher` changes
to the new certificate's subject. Windows treats a new publisher as a different
app, so the self-signed installation is uninstalled once and the new one
installed.

## Versions

The package version comes from the workspace version in `Cargo.toml`, with its
major part plus one, followed by the commit count. App Installer rejects a zero
major part, so Cargo `0.1.0` at commit 312 becomes package `1.1.0.312`. Use
`-Revision <n>` to override the commit count. **Settings › About › Version**
shows both, for example `0.1.0 (package 1.1.0.312)`.
The build ID, the commit with `-modified` when the working tree had uncommitted
changes, appears under **Settings › About › Build**.

`package.ps1` refuses to publish a version the channel already has, because
Windows wouldn't install changed contents under the same version. Commit first,
or pass a higher `-Revision`.

## Procedures

Every command runs from the repository root. The default channel is
`%LOCALAPPDATA%\CodexBar\channel`; pass `-Channel <folder>` to use another
folder, such as a network share other PCs can read.

| Task | Command |
| --- | --- |
| First install on a PC | `.\package.ps1 -Trust -Install` |
| Publish an update | `.\package.ps1` |
| Publish and install an update now | `.\package.ps1 -Install` |
| Install a published update now | **Settings › About › Install and restart**, or `Add-AppxPackage -AppInstallerFile <channel>\CodexBar.appinstaller` |
| Roll back | `.\package.ps1 -Rollback 1.1.0.311 -Install` |
| Renew the signing certificate | `.\package.ps1 -RenewCertificate -Trust`, then trust the new `CodexBar.cer` on every other PC |
| Install on another PC | `Import-Certificate -FilePath <channel>\CodexBar.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople` as administrator, then `Add-AppxPackage -AppInstallerFile <channel>\CodexBar.appinstaller` |

**Clean install.** `-Trust` trusts the certificate. `-Install` installs from the
App Installer file, which records the channel, so later updates find it. Start
CodexBar from the Start menu. It uses the same settings, keys and history as the
build from source (`%USERPROFILE%\.codexbar` and Credential Manager), and only
one of the two runs at a time. `-Install` therefore stops a running `run.ps1`
copy and starts the package in its place. If that copy started with Windows,
the `Run` entry is removed and the package's own startup task is turned on the
next time CodexBar starts. **Settings › General › Start with Windows** switches
it afterwards.

The executable links the C runtime statically, so the package needs no Visual
C++ Redistributable. `package.ps1` builds it in its own target folder
(`target\msix\build`) and refuses an executable that still imports
`VCRUNTIME140.dll`.

**Updates.** Publishing puts the new package next to the old ones and points the
App Installer file at it. Windows checks the file when CodexBar starts (and in
the background about every 8 hours), then installs the update. CodexBar also
checks quietly 30 seconds after it starts and once a day after that, and shows
the result under **Settings › About › Updates**. When an update is waiting,
**Install and restart** installs it and Windows starts CodexBar again. A failed
check shows Windows' reason and can be retried with **Check now**. A missing or
unreadable channel never stops CodexBar from starting.

**Rollback.** `-Rollback <version>` points the App Installer file at an earlier
package that is still in the channel folder. Nothing is rebuilt. The file allows
moving to any version, so every installation goes back the next time CodexBar
starts. `-Install` does it right away on this PC, through the App Installer file
so the installation keeps its channel. To move forward again, publish
a newer build. Keep old packages in the channel for as long as you might roll
back to them.

## Widgets

Only packaged apps can provide Windows widgets, and the only widget host is the
Windows 11 Widgets board. The package declares one widget, **CodexBar usage**, in
three sizes. The board lists widgets from a self-signed (sideloaded) package
only with **Developer Mode** on (Settings › System › For developers). Settings ›
Widgets and `package.ps1 -Install` say when it is off, and Settings says when
the PC runs Windows 10. Add the widget from the Widgets board (Windows key + W,
**Add widgets**). The picker shows a sample medium widget, drawn by
`package.ps1`.
**Customize widget** chooses what it shows and the layout.
It can show **My tiles** (below), all accounts in dashboard order, one provider
or one group. The layout is automatic (one tile on small, two on medium, four
on large, or all six of My tiles on large) or one, two or four tiles.

**Settings › Widgets** is the widget builder (#95):

- **My tiles.** Up to six tiles, in order. Each is one account's limit or
  balance, with a display mode:
  - **Automatic**: a bar for limits, the amount for money.
  - **Compact percentage**: the percentage, large.
  - **Full bar**: name, value, bar and reset time.
  - **Balance only**: the money left or spent.
  - **Urgent status**: OK, Watch, At risk or Limit soon, in color.

  Each tile's menu also moves it up or removes it. **Add a tile** lists every
  limit and balance CodexBar has fetched.
- **Widget refresh.** When CodexBar refreshes (the default), or every 5 or 15
  minutes. Widgets redraw at that pace; their usage still comes from
  CodexBar's own refreshes.
- **Preview.** Shows the automatic layout at all three sizes, or the one-,
  two- or four-tile layout, from the latest usage. It works in the build from
  source and in the demo too.
- **Reset.** Removes every tile and restores the default refresh, after
  confirming.

The tiles and the refresh choice are saved in `dashboard.json`. They reach the
widgets with the next `widgets.json`, which is written as soon as they change.
Tapping a tile opens CodexBar on that account, in Usage, with History set to
the tile's metric. If CodexBar is already running, the new launch hands the
request to it and exits. A tile whose account was removed reads **Removed
account**, and one whose limit is no longer reported says so. Both point back to
Settings › Widgets. Renamed accounts show their new name, because tiles keep
the account's id.

- **Data.** The dashboard writes `widgets.json` next to `settings.json` after
  every refresh and at least every five minutes. The file holds names, group
  names, formatted values, percentages and times only, never keys, tokens or
  error messages. Its format is versioned: a newer CodexBar can add fields, and
  a widget older than the file says so instead of misreading it.
- **Provider.** When the board needs CodexBar, Windows starts
  `codexbar.exe -RegisterProcessAsComServer`. That process opens no window and
  reads no credentials. It runs beside the tray app, redraws visible widgets
  when `widgets.json` changes and once a minute, and exits after ten minutes
  with no widget on screen.
- **States.** Before the first snapshot, the widget asks you to open CodexBar.
  A limit with a pace carries the share projected by its reset in `widgets.json`
  (`projectedPercent`, past 100 when it runs out first), and the tile's line says
  it ("Weekly: 42% used, on pace for 78%"); Adaptive Cards can't fade a bar.
  Accounts whose last refresh failed show **Last known** with their age, and
  accounts with no usage yet show **Unavailable**. A snapshot older than 15
  minutes reads **CodexBar isn't running**, with an Open button. Tapping the
  widget outside a tile opens CodexBar. A provider or group that no longer exists shows a
  message pointing to **Customize widget**.
- **Windows App Runtime.** The provider uses the Windows App SDK's widget
  manager, so the package depends on the `Microsoft.WindowsAppRuntime.2`
  framework. `package.ps1` takes it from Microsoft's
  `Microsoft.WindowsAppSDK.Runtime` NuGet package, checks Microsoft's signature,
  copies it into the channel and lists it in the App Installer file. Windows
  installs it with CodexBar where it is missing.
- **Bindings.** `crates/codexbar-app/src/widgets/bindings.rs` is generated from
  the Widgets metadata by `scripts/Update-WidgetBindings.ps1`, with pinned
  versions.

## macOS and Linux

The macOS and Linux packages (#167) are built from source by two scripts at the repository root. CI builds both and
keeps them as artifacts of the Package workflow. Neither has an update channel yet: install a newer one over the old.

### macOS: CodexBar.app and a DMG

```bash
./package-macos.sh                        # target/package/macos/CodexBar.app and CodexBar-<version>.dmg
CODESIGN_IDENTITY="Developer ID Application: …" ./package-macos.sh
```

It needs Rust and the Xcode Command Line Tools. The app is a menu-bar app (`LSUIElement`) with the bundle id
`HemSoft.CodexBar` and an icon drawn by `cargo run -p codexbar-app --example icon`. Without `CODESIGN_IDENTITY` it is
signed ad hoc: it runs on the Mac that built it, but Gatekeeper refuses it on another Mac until it is opened once with
Control-click › Open (or `xattr -dr com.apple.quarantine CodexBar.app`). A Developer ID and notarization are the
production choice and are left open.

Open the DMG and drag CodexBar to Applications. The first alert (or Settings › Alerts › Send a test notification)
asks for permission to show notifications. Allow it, and alerts appear in Notification Center from then on.

### Linux: a .deb

```bash
./package-linux.sh                        # target/package/linux/codexbar_<version>_<arch>.deb
sudo apt install ./target/package/linux/codexbar_*.deb
```

It needs Rust, `dpkg-deb` and the build headers listed in the README. The package installs `/usr/bin/codexbar`, the
desktop entry `HemSoft.CodexBar.desktop` (the window's app id, so desktops show its name and icon) and the icon in
four sizes. It depends on the libraries GPUI loads: xcb, xkbcommon, Vulkan and Wayland. A Vulkan driver (Mesa's,
recommended) draws the window. Remove it with `sudo apt remove codexbar`; settings in `~/.codexbar` stay.

On Wayland the tray icon raises the window but doesn't hide it, and Close leaves it open: a compositor may refuse
to bring back a minimized window for a tray click, so CodexBar never minimizes there.

## Uninstall

1. Quit CodexBar from the tray menu.
2. Remove the app: **Settings › Apps › Installed apps › CodexBar › Uninstall**,
   or `Get-AppxPackage HemSoft.CodexBar | Remove-AppxPackage`.
3. Remove the trust, as administrator:
   `Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq 'CN=HemSoft CodexBar Self-Signed' | Remove-Item`.
4. Optionally remove the signing certificate:
   `Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -eq 'CN=HemSoft CodexBar Self-Signed' | Remove-Item`.
   Then delete the channel folder.

Settings, keys and history stay. The README's uninstall steps remove them.

## Verification

[`scripts/Test-MsixChannel.ps1`](../scripts/Test-MsixChannel.ps1) runs these
procedures end to end in a throwaway channel:

1. Clean install from the App Installer file.
2. The installed app reports its version and channel (`codexbar --package-status`,
   run inside the package), and Windows starts the widget provider for its COM
   class.
3. A published build is seen by the app's own update check, then installed.
4. Rollback, which keeps the channel.
5. Uninstall.

The **Package** workflow runs it on a clean Windows runner for every pull request
that touches packaging. It refuses to run on a PC where CodexBar is installed,
because it would replace and then remove that installation. It runs as
administrator. Afterwards it removes the certificate trust it added and any
signing certificate it created.
