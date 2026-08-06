[CmdletBinding()]
Param(
    [Parameter()][Alias('i')][switch]$Install,
    [Parameter()][Alias('h')][switch]$Help,
    [Parameter()][switch]$SkipRemoteServer,
    [Parameter()][Alias('a')][ValidateSet('x86_64', 'aarch64')][string]$Architecture
)

$ErrorActionPreference = 'Stop'

if ($Help) {
    Write-Output "Usage: bundle-windows-custom.ps1 [-Architecture <x86_64|aarch64>] [-SkipRemoteServer] [-Install]"
    Write-Output "Build a stable Windows installer with the Preview application icon."
    exit 0
}

$releaseChannelPath = Join-Path $PSScriptRoot "..\crates\zed\RELEASE_CHANNEL"
$releaseChannel = (Get-Content $releaseChannelPath -Raw).Trim()
if ($releaseChannel -ne 'stable') {
    throw "Custom Windows bundles must use the stable release channel; found '$releaseChannel'."
}

$bundleArguments = @{
    IconChannel = 'preview'
}
if ($Architecture) {
    $bundleArguments.Architecture = $Architecture
}
if ($Install) {
    $bundleArguments.Install = $true
}
if ($SkipRemoteServer) {
    $bundleArguments.SkipRemoteServer = $true
}

$workspace = Resolve-Path (Join-Path $PSScriptRoot '..')
Push-Location $workspace
try {
    & "$PSScriptRoot\bundle-windows.ps1" @bundleArguments
}
finally {
    Pop-Location
}
