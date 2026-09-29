# Microsoft Store package

The same exe the setup.exe and the MSI install is also packed into an MSIX for the Microsoft Store.
The Store signs what it publishes, so Store installs do not get the SmartScreen warning the
unsigned installers do (CODE_SIGNING.md).

## How it is built

- `packaging/msix/AppxManifest.xml` - the package manifest. Identity `monsama.NOBSSQLEditor`,
  publisher `CN=83AA0FC0-032F-4377-A730-A1EA43146C8F`, as Partner Center reserved them
  (Product management > Product identity). The Store refuses a package whose values differ.
- `packaging/msix/Assets/` - the logos, made from `src-tauri/icons/icon.png`.
- `packaging/msix/pack.ps1` - copies the built exe and the assets into a layout folder, fills in
  the version and packs it with the Windows SDK's makeappx. The version gets a fourth part, 0:
  1.6.3 is 1.6.3.0, which is what the Store requires.
- `build.yml` runs it on every tag and keeps the `.msix` as the `microsoft-store-msix` artifact of
  the run. It is not put on the GitHub release: unsigned, it cannot be installed from a download.

Locally, after `npm run tauri build -- --no-bundle`: `pwsh packaging/msix/pack.ps1` writes
`dist/NOBS.SQL.Editor_<version>.0_x64.msix`. To run it on a machine with Developer Mode on, without
signing, register the layout folder: `Add-AppxPackage -Register dist/msix-layout/AppxManifest.xml`.

## What is different in the Store copy

The exe finds out at startup whether it runs from a package (`store_package` in `main.rs`) and then:

- **Updates come from the Store.** The update notice, the "Check for updates" settings and
  `update_install` are off. Settings > Updates says the Store installs them.
- **No client tool download.** Store policy does not allow an app to download programs that extend
  it, so the "Download MariaDB / MySQL client tools" buttons are hidden. Tools that are installed
  or selected by path are found and used as in any other install.
- **Settings live in the package.** Windows keeps what a packaged app writes under AppData in
  `%LOCALAPPDATA%\Packages\monsama.NOBSSQLEditor_<id>\LocalCache`. The app sees that merged with
  the usual folders, so a setup.exe install's saved connections show up in the Store copy - but
  what either one changes afterwards, the other does not see, and uninstalling the Store copy
  deletes its settings. "Open folder" sends Explorer to the package's copy.
- **Passwords are shared.** They are in Windows Credential Manager, which is not virtualized.
- **WebView2 is not bundled.** It is part of Windows 11 and of every updated Windows 10, and the
  package requires Windows 10 1809 or later.

## To check on a real install

- An SSH tunnel with a password: ssh.exe asks this exe for it (SSH_ASKPASS), started from inside
  the package's install folder.
- Export and Import with tools selected by path.
- "Open folder" for the settings and the tools.

## Submitting

In Partner Center, under the reserved app, a new submission needs:

1. **Packages** - the `.msix` from the `microsoft-store-msix` artifact of the tag's build run.
2. **Pricing and availability** - free, markets.
3. **Properties** - category Developer tools; a privacy policy URL is required, because the app
   stores credentials and connects to servers.
4. **Age ratings** - the questionnaire.
5. **Store listings** - description and at least one screenshot.
6. **Submission options** - the `runFullTrust` capability needs a reason: a desktop database
   client that runs the mysql and mysqldump tools and ssh.exe, and reads and writes files the user
   picks.
