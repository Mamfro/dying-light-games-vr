[CmdletBinding(SupportsShouldProcess=$true)]
param([ValidateRange(256,8192)][int]$Width,[ValidateRange(256,8192)][int]$Height,[switch]$Restore,
    [string]$SettingsPath=(Join-Path ([Environment]::GetFolderPath('MyDocuments')) 'DyingLight\out\settings\video.scr'))
# Dying Light 1's render resolution, for VR. Each eye is rendered at the game's resolution, which is 16:9 for a roughly
# square eye view; a resolution with the eye's own aspect wastes fewer pixels and is sharper. The producer logs the size
# that matches the headset ("a WxH game resolution would match it" in monaka_dl1.log).
#   .\Set-DL1Resolution.ps1                      shows the current resolution
#   .\Set-DL1Resolution.ps1 -Width 2880 -Height 2880   sets it (the first change remembers the original)
#   .\Set-DL1Resolution.ps1 -Restore             puts the original back
# The game must be closed: it rewrites video.scr on exit.
$ErrorActionPreference='Stop'
$path=(Resolve-Path -LiteralPath $SettingsPath).Path
$original="$path.monaka_resolution"
$bytes=[IO.File]::ReadAllBytes($path)
if ($bytes.Length -ge 2 -and (($bytes[0] -eq 255 -and $bytes[1] -eq 254) -or ($bytes[0] -eq 254 -and $bytes[1] -eq 255))) { throw 'UTF-16 settings are unsupported.' }
$bom=$bytes.Length -ge 3 -and $bytes[0] -eq 239 -and $bytes[1] -eq 187 -and $bytes[2] -eq 191
$content=[IO.File]::ReadAllText($path)
$pattern='(?m)^([\t ]*)Resolution\(\s*(\d+)\s*,\s*(\d+)\s*\)'
$match=[regex]::Match($content,$pattern)
if (-not $match.Success) { throw "No active Resolution(w,h) line in $path." }
$current="$($match.Groups[2].Value)x$($match.Groups[3].Value)"
if (-not $Restore -and -not $PSBoundParameters.ContainsKey('Width')) {
    [pscustomobject]@{ SettingsPath=$path; Resolution=$current; Original=$(if (Test-Path -LiteralPath $original) { (Get-Content -LiteralPath $original -Raw).Trim() } else { $current }) }
    return
}
if (Get-Process -Name DyingLightGame -ErrorAction SilentlyContinue) { throw 'Close Dying Light first: it rewrites video.scr on exit.' }
if ($Restore) {
    if (-not (Test-Path -LiteralPath $original)) { Write-Output "Nothing to restore; the resolution is $current."; return }
    $target=(Get-Content -LiteralPath $original -Raw).Trim()
    if ($target -notmatch '^(\d+)x(\d+)$') { throw "Unreadable $original." }
    $Width=[int]$Matches[1]; $Height=[int]$Matches[2]
} elseif (-not $PSBoundParameters.ContainsKey('Height')) { throw 'Give both -Width and -Height.' }
$updated=[regex]::Replace($content,$pattern,{ param($m) "$($m.Groups[1].Value)Resolution($Width,$Height)" },1)
if ($PSCmdlet.ShouldProcess($path,"Set Resolution($Width,$Height) (was $current)")) {
    if (-not $Restore -and -not (Test-Path -LiteralPath $original)) { Set-Content -LiteralPath $original -Value $current }
    [IO.File]::WriteAllText($path,$updated,[Text.UTF8Encoding]::new($bom))
    if ($Restore) { Remove-Item -LiteralPath $original }
    [pscustomobject]@{ SettingsPath=$path; Resolution="${Width}x$Height"; Was=$current }
}
