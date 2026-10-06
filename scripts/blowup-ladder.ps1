<#
.SYNOPSIS
  Drive the "Characterizing Datalog Rule Blowups" recipe (recipes.md) for one APK.

.DESCRIPTION
  1. Imports the APK once into a scratch store (XDG_STATE_HOME = <OutDir>/state).
  2. Runs `ctadl index` with no timeout under the memory guard: the baseline. Either it reaches a
     fixpoint, or the guard fires, and the wall time to the guard bounds the ladder.
  3. Runs `ctadl index` once per rung of -Timeouts with CTADL_INDEX_TIMEOUT_SECS set, still under
     the guard, at RUST_LOG=warn,ctadl=debug (minus the SSA IR dumps) so each log carries the scc/rule times, the relation
     census and the per-index sizes.

  Every run is under scripts/memguard.ps1, logs to <OutDir>/<label>.log, and ends with a
  [memguard] summary line. Feed the logs to scripts/blowup-rank.py.

.EXAMPLE
  scripts/blowup-ladder.ps1 -Apk xtask/tests/dex/com.noto_54.apk -LimitGB 4 -Timeouts 10,20,40 -OutDir tmp/blowup-noto
#>
param(
    [string] $Apk,
    [double] $LimitGB = 4,
    [int[]] $Timeouts = @(10, 20, 40, 80),
    [string] $OutDir,
    [string] $Name = 'app',
    [string] $Ctadl = 'target/release/ctadl.exe',
    [switch] $SkipImport,
    [switch] $SkipBaseline
)
if (-not $Apk -or -not $OutDir) { throw 'usage: blowup-ladder.ps1 -Apk <apk> -OutDir <dir> [-LimitGB 4] [-Timeouts 10,20,40]' }

$guard = Join-Path $PSScriptRoot 'memguard.ps1'
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$Ctadl = (Resolve-Path $Ctadl).Path
$env:XDG_STATE_HOME = Join-Path $OutDir 'state'

function Guarded([string] $label, [string[]] $ctadlArgs) {
    $log = Join-Path $OutDir "$label.log"
    Remove-Item $log -ErrorAction SilentlyContinue
    & $guard -LimitGB $LimitGB -LogFile $log $Ctadl @ctadlArgs
}

if (-not $SkipImport) {
    $env:RUST_LOG = $null
    Remove-Item Env:CTADL_INDEX_TIMEOUT_SECS -ErrorAction SilentlyContinue
    Guarded 'import' @('import', '-l', 'apk', '--name', $Name, (Resolve-Path $Apk).Path)
    if ($LASTEXITCODE -ne 0) { throw "import failed; see $OutDir\import.log" }
}

# `ctadl_ir::ssa` at debug dumps the IR of every function (98% of a 64 MB noto log); the profile
# comes from `ctadl_ascent::index_engine` and needs none of it.
$env:RUST_LOG = 'warn,ctadl=debug,ctadl_ir::ssa=info'
if (-not $SkipBaseline) {
    Remove-Item Env:CTADL_INDEX_TIMEOUT_SECS -ErrorAction SilentlyContinue
    Guarded 'baseline' @('index', $Name)
}
foreach ($t in $Timeouts) {
    $env:CTADL_INDEX_TIMEOUT_SECS = "$t"
    Guarded ('t{0:D4}' -f $t) @('index', $Name)
}
Remove-Item Env:CTADL_INDEX_TIMEOUT_SECS -ErrorAction SilentlyContinue
