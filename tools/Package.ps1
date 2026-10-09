[CmdletBinding()]
param()
# Makes the Dying Light game pack: dist\dying-light-games-vr-<version>.zip, holding a
# dying-light-games-vr folder with the three producer DLLs (Dying Light, Dying Light 2, The Beast),
# the DyingLight2VR notice the Dying Light 2 pack declares, the README and the licence. The folder's
# name stays the same from version to version, so adding a newer zip replaces the installed pack.
# Each DLL is read back with Monaka's monaka_play pack before it is zipped. The build is the release
# one of Monaka's tools\Release.ps1 (C runtime linked in, no paths of this PC inside). Upload the zip
# to a GitHub release of the same version; nothing here publishes anything.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
. (Join-Path $MonakaRoot 'tools\Release.ps1')
$repo = Split-Path -Parent $PSScriptRoot
$version = Get-WorkspaceVersion $repo
# The source root is the folder holding both monaka_vr and monaka_adapters.
$built = Invoke-ReleaseBuild -Workspace $repo -SourceRoot (Split-Path -Parent (Split-Path -Parent $repo)) -CargoArgs @('-p', 'monaka_dl1', '-p', 'monaka_dl2', '-p', 'monaka_beast')

$dist = Join-Path $repo 'dist'
$folder = Join-Path $dist 'dying-light-games-vr'
if (Test-Path -LiteralPath $folder) { Remove-Item -LiteralPath $folder -Recurse }
New-Item -ItemType Directory $folder -Force | Out-Null
$dlls = 'monaka_dl1.dll', 'monaka_dl2.dll', 'monaka_beast.dll'
foreach ($dll in $dlls) { Copy-Item -LiteralPath (Join-Path $built $dll) -Destination $folder }
Copy-Item -LiteralPath (Join-Path $repo 'licenses\DyingLight2VR-LICENSE.txt'), (Join-Path $repo 'README.md'), (Join-Path $repo 'LICENSE') -Destination $folder
Assert-NoPersonalPaths ($dlls | ForEach-Object { Join-Path $folder $_ })

# Read each DLL back as a pack, as the launcher will.
$play = Join-Path $MonakaRoot 'target\release\monaka_play.exe'
if (-not (Test-Path -LiteralPath $play)) { throw "Build Monaka first (missing $play)." }
foreach ($dll in $dlls) {
    $said = & $play pack (Join-Path $folder $dll) 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $said -notmatch 'this Monaka runs it') { throw "$dll is not a pack this Monaka runs:`n$said" }
    Write-Output $said.Trim()
}

$zip = New-ReleaseZip -Folder $folder -Zip (Join-Path $dist "dying-light-games-vr-$version.zip")
Write-Output "Dying Light pack $version`: $zip"
Get-ChildItem -LiteralPath $folder | ForEach-Object { Write-Output ("  {0,-28} {1,10:N0} bytes" -f $_.Name, $_.Length) }
