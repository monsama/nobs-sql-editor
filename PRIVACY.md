# Privacy policy

NOBS SQL Editor is a desktop client for MySQL and MariaDB, made by monsama. This applies to every
edition: the setup.exe, the MSI, the Microsoft Store version and the PowerShell edition.

## What the app collects

Nothing. There is no telemetry, no analytics, no crash reporting and no account. monsama receives
no data from the app, and nothing you do in it is sent anywhere except where you tell it to go.

## What stays on your computer

- **Saved connections** - host, port, user name, SSL and SSH settings - in the app's settings
  folder in your Windows profile.
- **Passwords** for connections and SSH, under your Windows account: in Windows Credential Manager,
  or in the PowerShell edition encrypted with Windows DPAPI in its settings.
- **Query history, the query library and open tabs**, in the same settings folder and in the app's
  own browser storage.

Settings > Data > "Clear all app data" deletes all of it, passwords included. Uninstalling the
Microsoft Store version deletes what it created in its own package folder.

## What the app connects to

- **The database servers and SSH hosts you connect to.** What goes there - your credentials, your
  queries, the data you change - is between you and that server.
- **GitHub (`api.github.com`)**, once at startup, to see whether a newer version exists. This can
  be switched off in Settings. The Microsoft Store version does not do this; the Store updates it.
- **GitHub, mariadb.org or mysql.com**, only when you ask the app to install an update or download
  the MySQL or MariaDB client tools. The Microsoft Store version does neither.

These requests carry nothing beyond what any web request does: your IP address and a user agent
naming the app. Links to monsama.ch and elsewhere open in your browser, and only when you click
them.

## Changes

Changes to this policy are made in this file, and its history on GitHub shows each one.

## Contact

Questions about this policy: open an issue at https://github.com/monsama/nobs-sql-editor/issues, or
reach monsama through https://monsama.ch.
