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

$Dir = ($Output | ForEach-Object { "$_" } | Where-Object { $_ -like "installing to *" } |
    Select-Object -Last 1) -replace "^installing to ", ""
$Executable = Join-Path $Dir "$Binary.exe"
if (-not ($Dir -and (Test-Path $Executable))) {
    $Command = Get-Command $Binary -ErrorAction SilentlyContinue
    if (-not $Command) {
        Write-Warning "installed, but $Binary wasn't found to migrate its contexts and enable its completion: run ``$Binary context migrate`` and ``$Binary completion powershell --install --file `$PROFILE``"
        exit 0
    }
    $Executable = $Command.Source
}
# Contexts a previous version stored, moved to the current format now
# rather than on first use. Never fails the install: the binary does it
# itself on first use otherwise.
try {
    & $Executable context migrate
} catch {
    Write-Warning "couldn't migrate $Binary contexts: $_"
}

if ($env:NO_COMPLETION -and $env:NO_COMPLETION -ne "0") { exit 0 }
# Never fails the install: completion is a nicety.
try {
    & $Executable completion powershell --install --file $PROFILE
} catch {
    Write-Warning "couldn't enable $Binary completion: $_"
}
