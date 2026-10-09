[CmdletBinding(DefaultParameterSetName='List')]
param([Parameter(ParameterSetName='Save',Mandatory)][ValidatePattern('^\w+$')][string]$Save,
    [Parameter(ParameterSetName='Go',Mandatory)][ValidatePattern('^\w+$')][string]$Go,
    [Parameter(ParameterSetName='List')][switch]$List)
# Named spots in Dying Light 1, for debugging (getting back to where a cutscene plays). While VR (or a harness run) is
# attached with head aim on, -Save NAME keeps where the player stands and faces, -Go NAME puts the player back there;
# with neither, the spots saved so far are listed. Spots are kept in runs\dl1-spots.txt, so they last across runs and
# game restarts. The game must be running (not paused in a menu): the move happens at the player's next camera update.
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
$runs=Join-Path $MonakaRoot 'runs'
$spots=Join-Path $runs 'dl1-spots.txt'
if ($PSCmdlet.ParameterSetName -eq 'List') {
    if (-not (Test-Path -LiteralPath $spots)) { 'No spots saved yet (Spot-DL1.ps1 -Save NAME).'; return }
    foreach ($line in Get-Content -LiteralPath $spots) {
        $w=$line -split '\s+'
        if ($w.Count -ge 14) { '{0,-16} at {1:N1} {2:N1} {3:N1}' -f $w[0],[double]$w[5],[double]$w[9],[double]$w[13] }
    }
    return
}
# The live run: the newest DL1 run folder whose log is still being written.
$run=Get-ChildItem -LiteralPath $runs -Directory -Filter 'dl1-*' | Sort-Object LastWriteTime -Descending |
    Where-Object { $log=Join-Path $_.FullName 'monaka_dl1.log'; (Test-Path -LiteralPath $log) -and -not (Select-String -LiteralPath $log -Pattern '^\s*[\d.]+ stopped$' -Quiet) } |
    Select-Object -First 1
if (-not $run) { throw 'No Dying Light 1 run is attached (start VR first).' }
$command=Join-Path $run.FullName 'spot-command.txt'
$text=if ($Save) { "save $Save" } else { "go $Go" }
Set-Content -LiteralPath $command -Value $text -Encoding ascii
$log=Join-Path $run.FullName 'monaka_dl1.log'
$before=(Get-Content -LiteralPath $log).Count
for ($i=0; $i -lt 30 -and (Test-Path -LiteralPath $command); $i++) { Start-Sleep -Milliseconds 100 }
if (Test-Path -LiteralPath $command) {
    Remove-Item -LiteralPath $command
    throw 'The game did not take the command within 3 s (paused in a menu, in a cutscene, or head aim off?).'
}
Start-Sleep -Milliseconds 100
Get-Content -LiteralPath $log | Select-Object -Skip $before | Where-Object { $_ -match ' spot ' }
