# Installs rmra or remora-etcher from a remora-companion release, then
# enables its PowerShell completion:
#
#   powershell -ExecutionPolicy Bypass -c "& ([scriptblock]::Create((irm https://github.com/sntns/remora-companion/releases/latest/download/install.ps1))) rmra"
#
# The binary comes from the release's own installer (dist's
# <binary>-installer.ps1); the completion from `<binary> completion
# powershell --install`, which adds one guarded line to $PROFILE, once.
# $env:NO_COMPLETION = "1" skips that part.
param([string]$Binary = "rmra")
$ErrorActionPreference = "Stop"

$Release = "@RELEASE@"
if ($Binary -notin @("rmra", "remora-etcher")) {
    Write-Error "install.ps1: unknown binary '$Binary' (rmra or remora-etcher)"
    exit 2
}
# A script published with a release installs that release; one run from the
# repository, the latest.
if ($Release.StartsWith("@")) {
    $Base = "https://github.com/sntns/remora-companion/releases/latest/download"
} else {
    $Base = "https://github.com/sntns/remora-companion/releases/download/$Release"
}

# The installer says where it put the binary (on the information stream).
$Output = & ([scriptblock]::Create((Invoke-RestMethod "$Base/$Binary-installer.ps1"))) 6>&1
$Output | ForEach-Object { Write-Host $_ }

if ($env:NO_COMPLETION -and $env:NO_COMPLETION -ne "0") { exit 0 }
$Dir = ($Output | ForEach-Object { "$_" } | Where-Object { $_ -like "installing to *" } |
    Select-Object -Last 1) -replace "^installing to ", ""
$Executable = Join-Path $Dir "$Binary.exe"
if (-not ($Dir -and (Test-Path $Executable))) {
    $Command = Get-Command $Binary -ErrorAction SilentlyContinue
    if (-not $Command) {
        Write-Warning "installed, but $Binary wasn't found to enable its completion: run ``$Binary completion powershell --install --file `$PROFILE``"
        exit 0
    }
    $Executable = $Command.Source
}
# Never fails the install: completion is a nicety.
try {
    & $Executable completion powershell --install --file $PROFILE
} catch {
    Write-Warning "couldn't enable $Binary completion: $_"
}
