[CmdletBinding()]
param([ValidateRange(2,600)][int]$Seconds=60,[ValidateRange(0,60)][int]$DelaySeconds=3,[ValidatePattern('^[a-z_]+=[\w.,:\-]+$')][string[]]$Probe=@('probe_melee=1'))
# A flat probe of Dying Light 1, no headset and no stereo: attaches monaka_dl1.dll with flat=1 (only the gameplay probes'
# hooks; the game plays on the monitor as usual) for -Seconds, then stops it and prints its log. -Probe picks the
# probes (default probe_melee=1: every hit the game deals). The game pauses without focus: click into it first.
# Build first: cargo build --release -p monaka_loader -p monaka_dl1
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
$root=$MonakaRoot
$release=Join-Path $root 'target\release'
$loader=Join-Path $release 'monaka_loader.exe'
$producer=Join-Path $release 'monaka_dl1.dll'
foreach ($path in $loader,$producer) { if (-not (Test-Path -LiteralPath $path)) { throw "Missing $path; see the build line at the top of this script." } }
$game=@(Get-Process DyingLightGame -ErrorAction Stop)[0]

$version=[DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$session=Join-Path $root ('runs\dl1-probe-' + $version)
New-Item -ItemType Directory -Path $session -Force | Out-Null
$dll=Join-Path $session ('monaka_dl1-' + $version + '.dll')
Copy-Item -LiteralPath $producer -Destination $dll
Set-Content -LiteralPath (Join-Path $session 'dl1-options.txt') -Value (@('flat=1') + $Probe)

if ($DelaySeconds) { Write-Output "Attaching in $DelaySeconds seconds."; Start-Sleep -Seconds $DelaySeconds }
$attached=$false
try {
    & $loader start $game.Id $dll
    if ($LASTEXITCODE -ne 0) { throw 'Attachment failed; see the producer log below.' }
    $attached=$true
    Start-Sleep -Seconds $Seconds
} finally {
    if ($attached -and (Get-Process -Id $game.Id -ErrorAction SilentlyContinue)) {
        & $loader stop $game.Id $dll
        if ($LASTEXITCODE -ne 0) { Write-Warning 'Could not confirm hook shutdown; another version must not be attached.' }
    }
    Write-Output '--- producer log ---'
    Get-Content -LiteralPath (Join-Path $session 'monaka_dl1.log') -ErrorAction SilentlyContinue | Select-Object -Last 30
    Write-Output "Run folder: $session"
}
