<#
Runs every automated test that works on this machine, one after another, and sums up at the end.

  1. Frontend unit tests    npm test                    (Node; no server)
  2. Browser layout tests   npm run test:browser        (Node and Microsoft Edge; no server)
  3. Backend unit tests     cargo test                  (Rust; no server)
  4. Backend live tests     cargo test --include-ignored, once per server
  5. GUI tests              tests/gui/run.mjs, once per server, against the debug build

4 and 5 need servers. First of all, the script starts them with tests/ci/start-test-servers.ps1
(downloaded on the first run, reused after it), which loads the fixture into both, puts MariaDB's
client tools where the app looks for them, and says how to reach each server. The servers are left
running afterwards unless -StopServers is given.

  pwsh -File tests/run-all.ps1                       everything, servers included
  pwsh -File tests/run-all.ps1 -Build -StopServers   build the debug app first, stop the servers at the end
  pwsh -File tests/run-all.ps1 -NoServers -MariaDsn 127.0.0.1:3306:root:pw
                                                     use servers you already have

-Root, -MariaPort, -MysqlPort and -Password go to the server script (its defaults otherwise). With
-NoServers the DSNs and CAs come from the parameters, or the NOBS_CI_* variables; a server with no
DSN has its live and GUI runs skipped, and the summary says so. -Skip leaves stages out
(ui, browser, cargo, live, gui - e.g. -Skip browser,gui); -Build builds the debug app before the GUI
tests (otherwise an existing build is used).

Needs PowerShell 7 (pwsh): Windows PowerShell 5.1 stops the server script on openssl's status line.
#>
param(
  [string]$MariaDsn = $env:NOBS_CI_MARIADB_DSN,
  [string]$MysqlDsn = $env:NOBS_CI_MYSQL_DSN,
  [string]$MariaCa = $env:NOBS_CI_MARIADB_CA,
  [string]$MysqlCa = $env:NOBS_CI_MYSQL_CA,
  [string]$MysqlBin = $(if ($env:MYSQL_BIN) { $env:MYSQL_BIN } else { Join-Path $env:APPDATA 'NOBSSQL-Desktop\bin\mysql.exe' }),
  [string]$MysqldumpBin = $(if ($env:MYSQLDUMP_BIN) { $env:MYSQLDUMP_BIN } else { Join-Path $env:APPDATA 'NOBSSQL-Desktop\bin\mysqldump.exe' }),
  [string[]]$Skip = @(),
  [switch]$Build,
  [switch]$NoServers,
  [switch]$StopServers,
  [string]$Root,
  [int]$MariaPort,
  [int]$MysqlPort,
  [string]$Password
)
$ErrorActionPreference = 'Continue'
# Started with pwsh -File, "-Skip browser,gui" arrives as one string, not a list.
$Skip = @($Skip | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$repo = Split-Path -Parent $PSScriptRoot

# --- The servers, first ---------------------------------------------------------------------------
# The server script prints KEY=value lines for the steps after it; they are read off its output here.
if (-not $NoServers -and -not ($Skip -contains 'live' -and $Skip -contains 'gui')) {
  Write-Host "`n===== Starting the test servers =====" -ForegroundColor Cyan
  $sargs = @{}
  foreach ($k in 'Root', 'MariaPort', 'MysqlPort', 'Password') { if ($PSBoundParameters.ContainsKey($k)) { $sargs[$k] = $PSBoundParameters[$k] } }
  $vals = @{}
  try {
    & (Join-Path $PSScriptRoot 'ci\start-test-servers.ps1') @sargs | ForEach-Object {
      $line = "$_"; Write-Host $line
      if ($line -match '^([A-Z_]+)=(.*)$') { $vals[$Matches[1]] = $Matches[2] } }
  } catch { Write-Host "The servers could not be started: $_" -ForegroundColor Red; exit 1 }
  if (-not $vals.NOBS_CI_MARIADB_DSN) { Write-Host 'The server script did not say how to reach the servers.' -ForegroundColor Red; exit 1 }
  $MariaDsn = $vals.NOBS_CI_MARIADB_DSN; $MysqlDsn = $vals.NOBS_CI_MYSQL_DSN
  $MariaCa = $vals.NOBS_CI_MARIADB_CA; $MysqlCa = $vals.NOBS_CI_MYSQL_CA
  $MysqlBin = $vals.MYSQL_BIN; $MysqldumpBin = $vals.MYSQLDUMP_BIN
  $env:NOBS_CI_REMOTE_HOST = $vals.NOBS_CI_REMOTE_HOST
}
$app = Join-Path $repo 'src-tauri\target\debug\nobs-sql-editor.exe'
$results = [System.Collections.Generic.List[object]]::new()

function Stage([string]$name, [string]$key, [scriptblock]$run, [string]$why) {
  if ($Skip -contains $key) { $results.Add([pscustomobject]@{ Stage = $name; Result = 'skipped'; Note = 'left out with -Skip' }); return }
  if ($why) { $results.Add([pscustomobject]@{ Stage = $name; Result = 'skipped'; Note = $why }); return }
  Write-Host "`n===== $name =====" -ForegroundColor Cyan
  $t = [Diagnostics.Stopwatch]::StartNew()
  & $run
  $ok = $LASTEXITCODE -eq 0
  $results.Add([pscustomobject]@{ Stage = $name; Result = $(if ($ok) { 'passed' } else { 'FAILED' }); Note = '{0:n0} s' -f $t.Elapsed.TotalSeconds })
}

# What the live and GUI runs read, for one server. Set for the run and put back after it.
function WithServer([string]$dsn, [string]$ca, [scriptblock]$run) {
  $keep = @{ D = $env:NOBS_TEST_DSN; C = $env:NOBS_TEST_SERVER_CA; M = $env:MYSQL_BIN; P = $env:MYSQLDUMP_BIN }
  $env:NOBS_TEST_DSN = $dsn; $env:NOBS_TEST_SERVER_CA = $ca; $env:MYSQL_BIN = $MysqlBin; $env:MYSQLDUMP_BIN = $MysqldumpBin
  try { & $run } finally { $env:NOBS_TEST_DSN = $keep.D; $env:NOBS_TEST_SERVER_CA = $keep.C; $env:MYSQL_BIN = $keep.M; $env:MYSQLDUMP_BIN = $keep.P }
}

Push-Location $repo
try {
  Stage 'Frontend unit tests' 'ui' { npm test }
  Stage 'Browser layout tests' 'browser' { npm run test:browser }
  Stage 'Backend unit tests' 'cargo' { Push-Location src-tauri; try { cargo test } finally { Pop-Location } }

  $tools = if (-not ((Test-Path $MysqlBin) -and (Test-Path $MysqldumpBin))) { "client tools not found ($MysqlBin) - the export/import tests would fail" }
  foreach ($s in @(@{ N = 'MariaDB'; D = $MariaDsn; C = $MariaCa }, @{ N = 'MySQL'; D = $MysqlDsn; C = $MysqlCa })) {
    $noDsn = if (-not $s.D) { 'no DSN given - start the servers and pass -' + $(if ($s.N -eq 'MariaDB') { 'MariaDsn' } else { 'MysqlDsn' }) }
    # --test-threads=1: the live tests share one database.
    Stage "Backend live tests ($($s.N))" 'live' { WithServer $s.D $s.C { Push-Location src-tauri; try { cargo test -- --include-ignored --test-threads=1 } finally { Pop-Location } } } $(if ($noDsn) { $noDsn } else { $tools })
  }

  if ($Build -and $Skip -notcontains 'gui') {
    Write-Host "`n===== Building the debug app =====" -ForegroundColor Cyan
    npm run tauri build -- --debug
  }
  foreach ($s in @(@{ N = 'MariaDB'; D = $MariaDsn; C = $MariaCa }, @{ N = 'MySQL'; D = $MysqlDsn; C = $MysqlCa })) {
    $why = if (-not $s.D) { 'no DSN given' } elseif (-not (Test-Path $app)) { 'no debug build - run with -Build' }
    Stage "GUI tests ($($s.N))" 'gui' { WithServer $s.D $s.C { node tests/gui/run.mjs --app desktop --target $app } } $why
  }
} finally { Pop-Location }

if ($StopServers -and -not $NoServers) {
  Write-Host "`n===== Stopping the test servers =====" -ForegroundColor Cyan
  # Shut down properly: killed mid-write, MariaDB's Aria logs are left damaged and it will not start
  # again on that data. Forced only if a server is still up after half a minute, and only the ones
  # started on the test folder - a server of your own is left alone. The client is MariaDB's, which
  # signs in to MySQL's root (caching_sha2_password) only with the plugin the start script puts
  # beside it; without --plugin-dir MySQL refused the SHUTDOWN and was always forced.
  $pluginDir = Join-Path (Split-Path $MysqlBin) 'plugin'
  foreach ($dsn in @($MariaDsn, $MysqlDsn) | Where-Object { $_ }) {
    $h, $p, $u, $pw = $dsn.Split(':', 4)
    & $MysqlBin --protocol=TCP "-h$h" "-P$p" "-u$u" "-p$pw" "--plugin-dir=$pluginDir" -e 'SHUTDOWN' 2>$null
  }
  $ci = if ($Root) { $Root } else { 'C:\nobs-ci' }
  $mine = { Get-CimInstance Win32_Process -Filter "Name='mariadbd.exe' OR Name='mysqld.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($ci) } }
  for ($i = 0; $i -lt 60 -and (& $mine); $i++) { Start-Sleep -Milliseconds 500 }
  & $mine | ForEach-Object { Write-Host "forcing $($_.Name) to stop"; Stop-Process -Id $_.ProcessId -Force }
}

Write-Host "`n===== Summary =====" -ForegroundColor Cyan
$results | Format-Table -AutoSize | Out-String | Write-Host
$failed = @($results | Where-Object Result -eq 'FAILED').Count
if ($failed) { Write-Host "$failed stage(s) failed - see their output above." -ForegroundColor Red; exit 1 }
Write-Host 'Every stage that ran passed.' -ForegroundColor Green
