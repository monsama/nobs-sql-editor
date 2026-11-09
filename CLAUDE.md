# NOBS SQL Editor

A desktop client for MySQL (5.7 to 9.4) and MariaDB (10.2 to 12.3) on Windows. It talks to the
server directly over the MySQL protocol. Only Windows is tested and released.

## Architecture

- **Tauri 2**: a Rust backend with an HTML/JS frontend in WebView2.
- **Backend**: Rust, the `mysql` crate for connections, `keyring` for passwords (Windows Credential
  Manager). Export and import run the `mysql` / `mysqldump` command-line tools.
- **Frontend**: one file, plain HTML/JS/CSS, no framework and no build step.

| Folder | What it holds |
|---|---|
| `ui/index.html` | The whole frontend |
| `src-tauri/src/main.rs` | Most of the backend, with its unit and live tests |
| `tests/ui` | Frontend unit tests (Node) |
| `tests/browser` | Layout tests in Microsoft Edge |
| `tests/gui/scenarios` | GUI tests that drive the built app |
| `tests/ci` | Scripts that start local test servers |
| `.github/workflows` | CI, compat tests and releases |
| `docs` | Testing and packaging notes |

## Testing locally

Needs Rust (MSVC toolchain), Node.js and PowerShell 7 (`pwsh`).

```bash
npm install
pwsh -File tests/run-all.ps1 -Build
```

This runs everything: frontend unit, browser, backend unit, backend live and GUI tests. It first
starts MariaDB 12.3 (port 3306) and MySQL 8.4 (port 3308) with `tests/ci/start-test-servers.ps1`,
then runs the live and GUI tests once against each.

- Leave stages out with `-Skip ui,browser,cargo,live,gui`.
- `-StopServers` stops the servers at the end.
- Older and newer servers: `tests/ci/start-compat-servers.ps1`.
- Without a server: `npm test`, `npm run test:browser`, and `cargo test` in `src-tauri`.
- `cargo clippy --all-targets -- -D warnings` must pass.

## Features

- **Connections**: saved profiles with colors and labels, read-only mode checked in the backend,
  a statement time limit, SSH tunnels, SSL/TLS up to full verification, PAM and LDAP sign-in.
- **Editor**: tabs with syntax highlighting and autocomplete, open and save `.sql` files, find
  and replace, run all, the selection or one statement, an output log, a session per tab, a
  visual Explain and Measure, a warning before UPDATE/DELETE without WHERE, server warnings,
  query history and a query library.
- **Results**: grids with filter, sort and lazy loading; inline and full-row edits applied in one
  transaction; multi-cell edits; a Value panel; go to a referenced row; manual transactions;
  charts; reading rows in another character set.
- **Databases**: browse all object types, a table designer, DDL and routine editing, table
  inspect and maintenance, ER diagrams, a server overview and the process list.
- **Compare DB**: schema sync and row compare between two databases, on one server or two.
- **Import and export**: CSV, INSERT, Excel, JSON and Markdown export; strict CSV import; database
  dump and restore through the command-line tools, as one file, a file per database or per table.
- **Users and privileges**: accounts, privileges per level, roles, account options, who has
  access to a database, and a transfer script to another server.
- **Updates**: a check for new releases, with a checked install.
