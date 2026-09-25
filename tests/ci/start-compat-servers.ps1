<#
Starts the servers the compatibility suite runs against - versions and settings the main suite
(tests/ci/start-test-servers.ps1: current MariaDB and MySQL) does not cover - and loads the shared
fixture into each. Used by .github/workflows/compat.yml; runnable locally on spare ports:

  pwsh -File tests/ci/start-compat-servers.ps1 -Root C:\nobs-compat

Each server is "name:flavor:version:port[:option,...]". Options:
  lctn2   lower_case_table_names=2 - table names are stored as created and compared ignoring case.
          It has to be set when the data directory is created, and it is only accepted on a file
          system that ignores case, which is why this suite runs on Windows.

What it writes (to $env:GITHUB_ENV when set, and always to the console), per server:
  NOBS_COMPAT_<NAME>_DSN   host:port:user:password
  NOBS_COMPAT_<NAME>_BIN   the server's own client tools (bin folder)
and, when it starts a single server, the same as NOBS_COMPAT_DSN and NOBS_COMPAT_BIN.

The apps' client tools are not this script's business: compat.yml runs start-test-servers.ps1
first, which puts current MariaDB and MySQL tools where the apps look - the tools a user has,
against a server older or newer than the one they were built with.
#>
param(
    [string]$Root = 'C:\nobs-compat',
    [string]$Password = 'CiTest.123',
    [string[]]$Servers = @('mysql57:mysql:5.7.44:3357', 'maria102:mariadb:10.2.7:3102', 'mysql94:mysql:9.4.0:3394:lctn2'),
    [string]$Fixture = (Join-Path $PSScriptRoot '..\fixtures\seed.sql')
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$cache = Join-Path $Root 'cache'
New-Item -ItemType Directory -Force $cache | Out-Null
# MariaDB 10.2.7's privilege tables (MyISAM, and Aria after the change below) were found corrupt or
# "marked as crashed" part way through runs on the CI runner, with no server crash in the log - the
# pattern of a file scanner holding a table file the server was writing, which that old release
# does not retry. The runner is disposable, so its scanner is told to leave these files alone. Only
# in CI: a developer's own machine keeps its settings.
if ($env:GITHUB_ACTIONS -eq 'true') {
    try { Add-MpPreference -ExclusionPath $Root -ErrorAction Stop; "Defender: excluded $Root" } catch { "Defender: no exclusion ($($_.Exception.Message))" }
}

function Get-Archive([string]$Name, [string[]]$Urls) {
    $file = Join-Path $cache $Name
    if (Test-Path $file) { return $file }
    foreach ($u in $Urls) {
        Write-Host "downloading $u"
        # dev.mysql.com and its CDN refuse browser-like user agents; curl's is accepted.
        & curl.exe -fsSL --retry 3 -A 'curl/8.0 NOBSSQL-CI' -o "$file.part" $u
        if ($LASTEXITCODE -eq 0) { Move-Item "$file.part" $file -Force; return $file }
        Remove-Item "$file.part" -Force -ErrorAction SilentlyContinue
    }
    throw "Could not download $Name"
}
function Expand-Flat([string]$Zip, [string]$Dest) {
    $tmp = Join-Path $Root ('x-' + [Guid]::NewGuid().ToString('N'))
    Expand-Archive -LiteralPath $Zip -DestinationPath $tmp -Force
    $inner = Get-ChildItem -LiteralPath $tmp -Directory | Select-Object -First 1
    New-Item -ItemType Directory -Force $Dest | Out-Null
    Get-ChildItem -LiteralPath $inner.FullName | Move-Item -Destination $Dest -Force
    Remove-Item $tmp -Recurse -Force
}
function Wait-Port([int]$Port, [string]$What) {
    for ($i = 0; $i -lt 180; $i++) {
        $c = New-Object Net.Sockets.TcpClient
        try { $c.Connect('127.0.0.1', $Port); return } catch { Start-Sleep -Milliseconds 500 } finally { $c.Dispose() }
    }
    throw "$What did not start listening on $Port"
}
function Invoke-Sql([string]$Client, [int]$Port, [string]$Sql, [string]$File, [switch]$NoPassword) {
    $cargs = @('--protocol=TCP', '-h127.0.0.1', "-P$Port", '-uroot', '--default-character-set=utf8mb4')
    if (-not $NoPassword) { $cargs += "-p$Password" }
    if ($File) {
        $p = Start-Process -FilePath $Client -ArgumentList $cargs -RedirectStandardInput $File -NoNewWindow -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "loading $File on port $Port failed ($($p.ExitCode))" }
    } else {
        & $Client @cargs -N -e $Sql
        if ($LASTEXITCODE -ne 0) { throw "'$Sql' on port $Port failed" }
    }
}
function First-Exe([string]$Bin, [string[]]$Names) {
    foreach ($n in $Names) { $p = Join-Path $Bin $n; if (Test-Path $p) { return $p } }
    throw "none of $($Names -join ', ') in $Bin"
}

$out = [ordered]@{}
foreach ($spec in $Servers) {
    $parts = $spec.Split(':')
    $name, $flavor, $version, $port = $parts[0], $parts[1], $parts[2], [int]$parts[3]
    $opts = if ($parts.Count -gt 4) { $parts[4].Split(',') } else { @() }
    $srvHome = Join-Path $Root "$flavor-$version"
    $data = Join-Path $Root "$name-data"
    $log = Join-Path $Root "$name.err"
    $extra = @(); if ($opts -contains 'lctn2') { $extra += '--lower-case-table-names=2' }

    if ($flavor -eq 'mariadb') {
        if (-not (Test-Path (Join-Path $srvHome 'bin'))) {
            $z = Get-Archive "mariadb-$version-winx64.zip" @(
                "https://archive.mariadb.org/mariadb-$version/winx64-packages/mariadb-$version-winx64.zip",
                "https://mirror.mariadb.org/mariadb-$version/winx64-packages/mariadb-$version-winx64.zip")
            Expand-Flat $z $srvHome
        }
        $bin = Join-Path $srvHome 'bin'
        # 10.2 still names them mysqld / mysql_install_db / mysql.
        $server = First-Exe $bin 'mariadbd.exe', 'mysqld.exe'
        $client = First-Exe $bin 'mariadb.exe', 'mysql.exe'
        if (-not (Test-Path $data)) {
            $install = First-Exe $bin 'mariadb-install-db.exe', 'mysql_install_db.exe'
            & $install "--datadir=$data" "--password=$Password" "--port=$port" '--allow-remote-root-access'
            if ($LASTEXITCODE -ne 0) { throw "$install failed" }
        }
        Start-Process -FilePath $server -WindowStyle Hidden -ArgumentList (@("--defaults-file=`"$data\my.ini`"", "--log-error=`"$log`"") + $extra)
        Wait-Port $port "$name ($flavor $version)"
    } else {
        $series = ($version.Split('.')[0..1]) -join '.'
        if (-not (Test-Path (Join-Path $srvHome 'bin\mysqld.exe'))) {
            $file = "mysql-$version-winx64.zip"
            $z = Get-Archive $file @(
                "https://cdn.mysql.com/Downloads/MySQL-$series/$file",
                "https://downloads.mysql.com/archives/get/p/23/file/$file")
            Expand-Flat $z $srvHome
        }
        $bin = Join-Path $srvHome 'bin'
        $server = Join-Path $bin 'mysqld.exe'; $client = Join-Path $bin 'mysql.exe'
        $fresh = $false
        if (-not (Test-Path $data)) {
            & $server '--initialize-insecure' "--basedir=$srvHome" "--datadir=$data" '--console' @extra
            if ($LASTEXITCODE -ne 0) { throw "mysqld --initialize failed for $name" }
            $fresh = $true
        }
        $sargs = @("--basedir=`"$srvHome`"", "--datadir=`"$data`"", "--port=$port", "--log-error=`"$log`"") + $extra
        if ([version]$version -ge [version]'8.0') { $sargs += '--mysqlx=OFF' }
        Start-Process -FilePath $server -WindowStyle Hidden -ArgumentList $sargs
        Wait-Port $port "$name ($flavor $version)"
        if ($fresh) {
            Invoke-Sql $client $port "ALTER USER 'root'@'localhost' IDENTIFIED BY '$Password'; CREATE USER 'root'@'%' IDENTIFIED BY '$Password'; GRANT ALL ON *.* TO 'root'@'%' WITH GRANT OPTION;" -NoPassword
        }
    }
    # MariaDB before 10.4 keeps its accounts in MyISAM tables, and 10.2.7 on Windows corrupted the
    # index of mysql.user in the middle of a run - locally and in CI, on a fresh data directory -
    # which then shows as doubled SHOW GRANTS lines and "Index for table 'user' is corrupt". Aria
    # (crash-safe, what 10.4 on uses for them) does not; that is the server's bug, not the app's.
    if ($flavor -eq 'mariadb' -and [version]$version -lt [version]'10.4') {
        $priv = 'user', 'db', 'tables_priv', 'columns_priv', 'procs_priv', 'proxies_priv', 'roles_mapping'
        Invoke-Sql $client $port ((($priv | ForEach-Object { "ALTER TABLE mysql.$_ ENGINE=Aria;" }) -join ' ') + ' FLUSH PRIVILEGES;')
    }
    Invoke-Sql $client $port -File (Resolve-Path $Fixture).Path
    Invoke-Sql $client $port 'SELECT VERSION(), @@lower_case_table_names, (SELECT COUNT(*) FROM nobs_test.ro_canary)'
    $key = $name.ToUpper()
    $out["NOBS_COMPAT_$($key)_DSN"] = "127.0.0.1:$($port):root:$Password"
    $out["NOBS_COMPAT_$($key)_BIN"] = $bin
    # One server - a matrix job's - is also named without its key, so the steps need not know it.
    if ($Servers.Count -eq 1) { $out['NOBS_COMPAT_DSN'] = "127.0.0.1:$($port):root:$Password"; $out['NOBS_COMPAT_BIN'] = $bin }
}
foreach ($k in $out.Keys) {
    "$k=$($out[$k])"
    if ($env:GITHUB_ENV) { Add-Content -Encoding utf8 -Path $env:GITHUB_ENV -Value "$k=$($out[$k])" }
}
