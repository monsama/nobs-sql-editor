# NOBS SQL Editor

[![Latest release](https://img.shields.io/github/v/release/monsama/nobs-sql-editor)](https://github.com/monsama/nobs-sql-editor/releases/latest)
[![test](https://github.com/monsama/nobs-sql-editor/actions/workflows/test.yml/badge.svg)](https://github.com/monsama/nobs-sql-editor/actions/workflows/test.yml)
[![compat](https://github.com/monsama/nobs-sql-editor/actions/workflows/compat.yml/badge.svg)](https://github.com/monsama/nobs-sql-editor/actions/workflows/compat.yml)
[![License: GPL v2+](https://img.shields.io/badge/license-GPL--2.0--or--later-blue)](LICENSE)

A desktop client for **MySQL** and **MariaDB** on Windows, built with
[Tauri](https://tauri.app) (a Rust backend with an HTML/JS frontend).

Only the Windows build is tested and released. Tauri also runs on macOS and Linux, so
you can build the app from source there (see [Building from source](#building-from-source)),
but there are no installers for those platforms and CI doesn't test them. Consider them
unsupported.

**Can't install software on your machine?** Use the
[PowerShell edition](https://github.com/monsama/nobs-sql-editor-powershell) instead. It has
the same interface, packed into a single `.ps1` script that starts a local server and opens
your browser. There is nothing to install, it needs no admin rights, and there is no
installer for SmartScreen to warn about. The trade-off is that it runs everything through
`mysql.exe`, so it needs the MySQL or MariaDB client tools, while the desktop app talks to
the server directly.

## At a glance

| | |
|---|---|
| **Platform** | Windows 10 or 11, 64-bit |
| **Runtime** | Microsoft Edge WebView2, part of Windows 10 and 11 (the installer fetches it if it is missing) |
| **Download** | 3.4 MB installer (`.exe`), or 4.7 MB `.msi` |
| **Servers** | MySQL 5.7 to 9.4 and MariaDB 10.2 to 12.3, tested - see [Supported servers](#supported-servers) |
| **Connection** | Direct, over the MySQL protocol; optional SSH tunnel; TLS with CA verification |
| **Passwords** | Windows Credential Manager, never in a file |
| **Your data** | `%APPDATA%\NOBSSQL-Desktop` (connections, settings, query library, log) |
| **Network** | Only your database servers, plus what is listed under [Network access](#network-access) |
| **License** | GPL-2.0-or-later |

## Supported servers

Every change is tested against these servers, with the full live and GUI test suites:

| Server | Versions tested |
|---|---|
| MySQL | 5.7, 8.0, 8.4, 9.4 (9.4 also with `lower_case_table_names=2`) |
| MariaDB | 10.2, 10.6, 10.11, 12.3 |

Versions in between are expected to work. Older ones (MySQL 5.6, MariaDB 10.1 and before) are
not tested. Where a server lacks a feature, the app leaves it out rather than failing: there are
no roles on MySQL 5.7, and no per-account password expiry or account locking on MariaDB before
10.4, so the user editor does not offer them there.

## Download

Windows installers are published on the
[Releases](https://github.com/monsama/nobs-sql-editor/releases) page.

They are **not code-signed**, so Windows SmartScreen shows a "Windows protected
your PC" warning naming an unknown publisher. Choose **More info -> Run anyway**
to continue.

Every release publishes `SHA256SUMS.txt` next to the installers and prints the same values in
its notes, so you can check that what you downloaded is what CI built:

```powershell
Get-FileHash .\NOBS.SQL.Editor_*_x64-setup.exe -Algorithm SHA256
```

[CODE_SIGNING.md](CODE_SIGNING.md) says what that does and does not prove, and why
there is no certificate.

### Uninstalling

1. To remove saved connections and their passwords as well, first open **Settings -> Clear all
   app data** in the app.
2. Uninstall **NOBS SQL Editor** from Windows **Settings -> Apps -> Installed apps** (or
   "Programs and Features").
3. Optionally delete what the app keeps for your user account:
   - `%APPDATA%\NOBSSQL-Desktop` - connections, settings, the query library, the log, and
     downloaded client tools;
   - `%LOCALAPPDATA%\ch.monsama.nobssqleditor` - the app window's browser data (open tabs,
     layout);
   - in Windows **Credential Manager** (Windows Credentials), any entries ending in
     `.NOBSSQL-Desktop` - saved connection passwords, if step 1 was skipped.

Export files are kept wherever you saved them.

## Features

**Connections**
- Saved connection profiles with a per-connection accent color and environment label.
- **Read-only / safe mode** for production servers: every statement is checked by the app's backend before it is sent, not only greyed out in the interface.
- SSH tunnels through the system's OpenSSH client (key, agent or password; host aliases and
  ProxyJump from `~/.ssh/config` work too).
- SSL/TLS modes up to full certificate verification, and PAM or LDAP sign-in over TLS.

**Editor**
- Tabbed SQL editor with syntax highlighting, and autocomplete that knows the tables and aliases
  of the statement.
- Find and replace (Ctrl+F / Ctrl+H), formatting, undo for every editor command.
- Run the whole script, the selection, or the statement at the cursor. A procedure call, or a
  script with several SELECTs, shows each result in a tab of its own.
- Explain draws the plan: every table read as a card, a full scan in red and an index lookup in
  green, with the joins, sorts and subqueries around them.
- Query history, and a reusable query library with export and import.

**Results and editing**
- Result grids with per-column filtering and sorting, column resize and show/hide, and a
  row-detail form for wide tables. Large results load as you scroll.
- Inline and full-row editing, staged as pending changes and applied in one transaction; add and
  delete rows. Right-click **Apply** (or Ctrl+Shift+S) shows the SQL it would run first.
- Pick several cells to type one value into all of them, or set them all to NULL or empty.
- **Go to referenced row** follows a foreign key, into another database too.
- Manual transactions: with Auto-commit off a tab keeps one transaction open across its runs
  until Commit or Rollback, with a log of what it has run.
- Charts: a result as bars or a line.
- Read the same rows in another character set, to tell text stored wrong from text read wrong.

**Schema**
- Browse schemas, tables, views, procedures, functions, triggers and events, with quick filtering.
- Table designer and DDL view and edit; routines and triggers edited and recreated in place.
- ER diagrams, and table maintenance (check, analyze, optimize, repair).
- Server overview (databases, sizes, row counts, character sets) and the process list, with kill.

**Compare DB**
- Schema sync between two databases, on the same server or two different ones: columns with
  their full definitions, indexes, foreign keys and CHECK constraints. Missing tables are created
  after the tables they refer to; drops are offered but left unticked.
- Row compare: rows missing on either side and rows that differ, copied or updated by their key.

**Import and export**
- Tables or query results to CSV or INSERT statements (streamed, for large tables), Excel, JSON or
  Markdown; copy as CSV, TSV or JSON.
- Strict CSV import.
- Database export and import through the MySQL/MariaDB command-line tools: structure and data,
  structure only or data only, as a file per table, per database or one file.

**Users and privileges**
- Privileges as a checklist per server, database or table, with the GRANT and REVOKE shown
  before they run.
- Roles and default roles; clone an account; sign-in method, SSL, password expiry and limits.
- Who has access to a database, and a transfer script that recreates accounts and roles on
  another server.

## Keeping data exact

- **Saving grid edits:** each change is checked, inside the same transaction, to match exactly
  one row. Otherwise nothing is saved. This catches a row changed or deleted since it was loaded,
  and a TIMESTAMP key in the hour the clocks go back (it shows the same as its neighbour). FLOAT
  keys are shown rounded, so they are matched by their text.
- **Reading in another character set (read-only).** A value that reads `cafÃ©` is either stored
  wrong or being read wrong, and a grid cannot tell you which. The server transcodes text into
  the session's character set before sending it, so the box beside the connection lets you read
  the same rows in another one: UTF-8 bytes stored in a latin1 column read as mojibake in utf8mb4
  and as themselves in latin1, while data that is genuinely damaged reads badly in both. Nothing
  can be written while this is on - what is shown is not what a write would store - and the
  connection itself is opened read-only at the server, not only in the app.
- **Grid edits go to the database the query ran in**, including after a leading `USE`.
- **Compare runs both connections in UTC.** TIMESTAMP values therefore copy correctly between
  servers in different time zones, and Compare shows them in UTC.
- **Copies name their columns** (Compare, Duplicate table, INSERT exports). Invisible columns are
  included; generated columns are left out, since the server computes them. CSV exports include
  every column, and the CSV import skips generated ones.
- **INSERT exports skip rows whose key already exists** (`ON DUPLICATE KEY UPDATE`), rather than
  using `INSERT IGNORE`, which would also cut short a value that does not fit.
- **The CSV import is strict.** Header names match the table's columns ignoring case. A column
  the table does not have, or a row with more or fewer fields than the header, imports nothing.
  Foreign key and unique checks stay on, and the whole file is one transaction.
- **A per-table export is one snapshot.** The whole database is dumped once and then split into
  one file per table or view, so the files are consistent with each other even while the database
  is being written to. Two tables whose names give the same file name get two files.
- **Schema sync writes each column as the source server defines it**, including its character
  set, collation, comment and generated expression.
- **Binary values are shown and saved as `0x…` hex**, byte for byte - BLOB, BINARY, BIT, spatial
  types and MySQL 9's VECTOR.

## SSL / TLS

Each connection has an SSL mode, and optionally a CA certificate (a `.pem` file) that the two
verifying modes check the server against.

| Mode | Encrypted | Certificate checked against the CA | Host name checked |
|---|---|---|---|
| `default` | when the server offers it | – | – |
| `disabled` | no | – | – |
| `required` | yes | no | no |
| `verify-ca` | yes | yes | no |
| `verify` | yes | yes | yes |

`required` and the verifying modes refuse a server without TLS rather than continue unencrypted.
`required` checks no certificate, so it protects against eavesdropping but not against someone
posing as the server; the verifying modes do both. Without a CA, the verifying modes check
against the Windows trust store.

**If the server uses the certificate MariaDB or MySQL generated for itself** - which is what you
get when nobody configured one - use **`verify-ca` with the server's CA**. That certificate is
self-signed, so no trust store accepts it, and it never names a real host (MySQL's is issued to
`MySQL_Server_<version>_Auto_Generated_Server_Certificate`), so `verify` refuses it even with the
right CA. The connection error says which of the two happened.

Where to get the CA: for MySQL it is `ca.pem` in the server's data directory. MariaDB's generated
certificate has no separate CA - use the certificate itself. Either can also be read off the
connection, which needs no access to the server's files:

```sh
echo | openssl s_client -starttls mysql -connect HOST:PORT -showcerts
```

The CA is the last certificate printed (for MariaDB, the only one).

**PAM and LDAP accounts** (MariaDB's `auth_pam`, MySQL Enterprise's PAM and LDAP plugins) sign in
with the password as it is typed. The app sends it only over an encrypted connection: `required`,
a verifying mode, or `default` when the server offers TLS. A MariaDB server has to ask for it that
way - set `pam_use_cleartext_plugin=ON` in its configuration. The app cannot answer PAM's other
way of asking (the dialog plugin), and the connection error says so.

Export and Import run the command-line client (below) with the same settings. The MariaDB client
has no way to check a CA without also checking the host name, except on connections to the local
machine, so there `verify-ca` is carried out as full `verify`. It never checks less than you asked
for - at worst a remote export fails where a query on the same connection works. On `required`,
MariaDB's dump tool is pinned to the certificate the server presented a moment before the export,
so the dump itself is encrypted or does not run.

## Client tools (mysql / mysqldump)

Export and Import use the official MySQL/MariaDB command-line tools. These are
**not bundled** with this application. On first use you can either point the app
at an existing install (Settings) or let it download the official MariaDB client
tools from mariadb.org on demand. The archive is checked against the SHA-256 that
MariaDB's own release API publishes for it before anything is unpacked, and a
mismatch installs nothing - the checksum comes from the API, not from the mirror
the bytes came from, so a redirected or altered download fails the check. If the
API lists no checksum, nothing is downloaded at all.

**MySQL servers get MySQL's own tools** when there are any: the two optional
"MySQL servers" paths in Settings, or else the newest MySQL Server installation
(`Program Files\MySQL\MySQL Server *\bin`). Export and Import ask the server
what it is and pick the pair to match; MariaDB servers, and MySQL servers on a
machine without MySQL's tools, use the default pair. It matters because MariaDB's
mysqldump writes values into a MySQL table's generated columns, which MySQL
refuses when the dump is restored. Without MySQL's tools such an export is
refused rather than written.

**No MySQL installed?** Settings can download MySQL's own `mysql` and `mysqldump`
(the current 8.4 LTS release from dev.mysql.com). MySQL publishes Windows binaries
only as the full server archive, so this is a ~270 MB download of which about
14 MB is kept, in `bin\mysql\`. The archive is checked against the MD5 on MySQL's
download page before anything is unpacked, and a mismatch installs nothing. If
MySQL moves its page or files, `mysql_download_page` and
`mysql_download_url_template` (with `{series}`, `{version}`, `{file_name}`) in
the config file override the defaults.

## Updates

A few seconds after it starts, the app asks GitHub (`api.github.com`) for the latest release of
[nobs-sql-editor](https://github.com/monsama/nobs-sql-editor/releases). If a newer version exists, a small
notice with a link appears in the bottom-left corner. Nothing is downloaded or installed.

Hide the notice with its **×** and it stays hidden until the next version. Switch the check off,
or run it by hand, under **Settings → Updates**.

## Network access

Apart from your database servers and SSH hosts, the app contacts:

| Host | When | What for |
|---|---|---|
| `api.github.com` | at start (can be switched off) | the update check above |
| `downloads.mariadb.org`, `dlm.mariadb.com` or a MariaDB mirror | only when you ask for it in Settings | MariaDB client tools |
| `dev.mysql.com`, `cdn.mysql.com`, `downloads.mysql.com` | only when you ask for it in Settings | MySQL client tools |

None of these requests carries anything beyond what any web request does: your IP address and a
user agent. There is no telemetry.

## How it's tested

Every push and pull request runs, on Windows:

- unit tests of the Rust backend and of the interface's logic (Node), and `clippy` with warnings
  as errors;
- live tests against real MariaDB and MySQL servers, and GUI tests that drive the built app
  through its interface - grid edits, Compare, export and import, the user editor;
- the same live and GUI tests against every server in [Supported servers](#supported-servers),
  and PAM sign-in against MariaDB with `auth_pam` (on Linux, in Docker).

They also run weekly, so a change on a vendor's download site is caught before a user meets it.
Each published release is checked afterwards: the checksums in its notes and in `SHA256SUMS.txt` must match the files as published.

## Building from source

Requirements: [Rust](https://rustup.rs) (with the MSVC toolchain on Windows),
[Node.js](https://nodejs.org), and the Tauri prerequisites for your platform.

```bash
npm install
npm run tauri dev     # run in development
npm run tauri build   # produce installers (NSIS .exe / MSI on Windows)
```

Tests: `cargo test` in `src-tauri` and `npm test` need no database. The live tests need one:
`tests/ci/start-test-servers.ps1` starts MariaDB and MySQL locally, and
`tests/ci/start-compat-servers.ps1` the older and newer versions listed above.

## License

This program is free software, licensed under the **GNU General Public License
version 2** (or, at your option, any later version). See [LICENSE](LICENSE).

Copyright (C) 2026 Viktor Ljuca - https://monsama.ch

## Third-party components

Built with Tauri and the Rust crates mysql, reqwest, zip, keyring, tokio, serde,
serde_json, regex, dirs, csv, hex, tempfile and chrono (and their dependencies),
each under its own license (mostly MIT / Apache-2.0). The MariaDB client tools,
when downloaded, are © MariaDB Foundation under GPLv2 and are obtained from
mariadb.org; they are not bundled with this application.
