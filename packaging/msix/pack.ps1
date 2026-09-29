# Packs the exe the Tauri build made into an MSIX for the Microsoft Store.
#
#   pwsh packaging/msix/pack.ps1 [-Exe src-tauri/target/release/nobs-sql-editor.exe] [-Out dist]
#
# The package is not signed: the Store signs what it publishes, and an unsigned MSIX cannot be
# installed from anywhere else, so it is not put on the GitHub release. To try one on this machine
# with Developer Mode on, register the folder instead: Add-AppxPackage -Register <layout>\AppxManifest.xml
param(
  [string]$Exe = 'src-tauri/target/release/nobs-sql-editor.exe',
  [string]$Out = 'dist'
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '../..')

# The Store wants four parts, and the fourth one 0: 1.6.3 is 1.6.3.0.
$version = (Get-Content (Join-Path $root 'package.json') -Raw | ConvertFrom-Json).version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw "package.json version '$version' is not x.y.z" }
$version = "$version.0"

$exePath = if ([System.IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $root $Exe }
if (-not (Test-Path $exePath)) { throw "no exe at $exePath - run the Tauri build first" }

$outDir = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
$layout = Join-Path $outDir 'msix-layout'
Remove-Item -Recurse -Force $layout -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $layout 'Assets') | Out-Null
Copy-Item $exePath (Join-Path $layout 'nobs-sql-editor.exe')
Copy-Item (Join-Path $PSScriptRoot 'Assets/*.png') (Join-Path $layout 'Assets')
$manifest = (Get-Content (Join-Path $PSScriptRoot 'AppxManifest.xml') -Raw).Replace('__VERSION__', $version)
[System.IO.File]::WriteAllText((Join-Path $layout 'AppxManifest.xml'), $manifest, (New-Object System.Text.UTF8Encoding $false))

# makeappx comes with the Windows SDK; the newest one installed is used.
$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" -ErrorAction SilentlyContinue |
  Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $makeappx) { throw 'makeappx.exe not found - install the Windows SDK' }

$msix = Join-Path $outDir "NOBS.SQL.Editor_${version}_x64.msix"
Remove-Item -Force $msix -ErrorAction SilentlyContinue
& $makeappx.FullName pack /d $layout /p $msix /o
if ($LASTEXITCODE -ne 0) { throw "makeappx failed with exit code $LASTEXITCODE" }
Write-Host "Packed $msix"
