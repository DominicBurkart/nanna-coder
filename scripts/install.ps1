<#
.SYNOPSIS
    Nanna Coder one-line installer for Windows.

.DESCRIPTION
    Windows is supported via WSL2. Native Nix + Podman on Windows is not viable,
    so this script bootstraps WSL2 + an Ubuntu distro and then runs the standard
    POSIX installer (scripts/install.sh) inside it.

    Requires Administrator the first time (only) — to enable WSL features and
    install a Linux distro. A clear UAC notification is printed before each
    privileged step. Subsequent runs (when WSL is already configured) do not
    require Administrator.

.PARAMETER SkipModelPull
    Skip the multi-GB Gemma 4 model download. Used by CI.

.PARAMETER NoStart
    Don't start the pod after install.

.PARAMETER Branch
    Branch to clone (default: main).

.PARAMETER Distro
    WSL distro name (default: Ubuntu).

.PARAMETER Yes
    Don't prompt; assume yes for elevation notices.

.EXAMPLE
    irm https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/scripts/install.ps1 | iex

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -SkipModelPull
#>
[CmdletBinding()]
param(
    [switch]$SkipModelPull,
    [switch]$NoStart,
    [string]$Branch = 'main',
    [string]$Distro = 'Ubuntu',
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'

function Write-Info  { param($m) Write-Host "==> $m" -ForegroundColor Blue }
function Write-Ok    { param($m) Write-Host "[OK] $m" -ForegroundColor Green }
function Write-Warn  { param($m) Write-Host "[!] $m"  -ForegroundColor Yellow }
function Write-Err   { param($m) Write-Host "[X] $m"  -ForegroundColor Red; throw $m }

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Notify-Elevation {
    param([string]$Component, [string]$Reason)
    Write-Host ''
    Write-Host '+- Administrator required ---------------------------' -ForegroundColor Yellow
    Write-Host "|  component: $Component"                              -ForegroundColor Yellow
    Write-Host "|  reason:    $Reason"                                 -ForegroundColor Yellow
    Write-Host '|  Windows will show a UAC prompt.'                    -ForegroundColor Yellow
    Write-Host '+----------------------------------------------------' -ForegroundColor Yellow
    Write-Host ''
    if (-not $Yes -and [Environment]::UserInteractive) {
        $ans = Read-Host 'Continue? [Y/n]'
        if ($ans -match '^(n|no)$') { Write-Err 'aborted by user' }
    }
}

# ---------- banner ----------

Write-Host ''
Write-Host 'Nanna Coder installer (Windows / WSL2)' -ForegroundColor Cyan
Write-Host "  branch:        $Branch"
Write-Host "  distro:        $Distro"
Write-Host "  skip model:    $([bool]$SkipModelPull)"
Write-Host "  start pod:     $(-not [bool]$NoStart)"
Write-Host ''
Write-Host 'On Windows, Nanna Coder runs inside WSL2. This installer will:'
Write-Host '  1. Enable WSL2 + the Virtual Machine Platform (Administrator)'
Write-Host "  2. Install the $Distro WSL distro if missing (Administrator)"
Write-Host '  3. Run the POSIX installer inside WSL (no Administrator)'
Write-Host ''

# ---------- 1. WSL feature + distro ----------

function Test-WslReady {
    if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) { return $false }
    try {
        # `wsl --status` exits 0 if WSL is installed and configured.
        $null = & wsl.exe --status 2>&1
        return ($LASTEXITCODE -eq 0)
    } catch { return $false }
}

function Test-DistroInstalled {
    param([string]$Name)
    if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) { return $false }
    # `wsl -l -q` outputs UTF-16; normalize.
    $list = (& wsl.exe -l -q 2>$null) -join "`n"
    return $list -match [regex]::Escape($Name)
}

if (-not (Test-WslReady) -or -not (Test-DistroInstalled $Distro)) {
    Notify-Elevation 'WSL2' "Enable Windows Subsystem for Linux and install the $Distro distro."
    if (-not (Test-Admin)) {
        Write-Warn 'Re-launching as Administrator...'
        $argList = @('-NoProfile','-ExecutionPolicy','Bypass','-File',$PSCommandPath)
        if ($SkipModelPull) { $argList += '-SkipModelPull' }
        if ($NoStart)       { $argList += '-NoStart' }
        if ($Yes)           { $argList += '-Yes' }
        $argList += @('-Branch',$Branch,'-Distro',$Distro)
        Start-Process -FilePath 'powershell.exe' -ArgumentList $argList -Verb RunAs -Wait
        exit $LASTEXITCODE
    }
    Write-Info "Installing WSL2 + $Distro (this may take several minutes and require a reboot)..."
    & wsl.exe --install --no-launch -d $Distro
    if ($LASTEXITCODE -ne 0) {
        Write-Err "wsl --install failed (exit $LASTEXITCODE). If this is a fresh install, reboot and re-run."
    }
    # First-time `wsl --install` typically requires a reboot before further wsl
    # commands work. Detect and instruct.
    if (-not (Test-WslReady)) {
        Write-Warn 'WSL has been installed but a reboot is required before continuing.'
        Write-Warn 'Please reboot Windows and re-run this script.'
        exit 0
    }
}

Write-Ok "WSL2 + $Distro ready"

# ---------- 2. delegate to install.sh inside WSL ----------

# Build the bash invocation. Pulling install.sh from the same branch so the
# Windows + Linux install paths stay versioned together.
$rawBase = "https://raw.githubusercontent.com/DominicBurkart/nanna-coder/$Branch/scripts/install.sh"

$bashFlags = @()
if ($SkipModelPull) { $bashFlags += '--skip-model-pull' }
if ($NoStart)       { $bashFlags += '--no-start' }
if ($Yes)           { $bashFlags += '--yes' }
$bashFlags += @('--branch', $Branch)
$bashArgs = ($bashFlags -join ' ')

$wslCmd = "set -euo pipefail; export NANNA_BRANCH='$Branch'; " +
          "command -v curl >/dev/null 2>&1 || { sudo apt-get update -y && sudo apt-get install -y curl ca-certificates git; }; " +
          "curl -fsSL '$rawBase' | bash -s -- $bashArgs"

Write-Info "Running POSIX installer inside WSL ($Distro)..."
Write-Host "    $wslCmd" -ForegroundColor DarkGray
& wsl.exe -d $Distro -- bash -lc $wslCmd
if ($LASTEXITCODE -ne 0) {
    Write-Err "WSL installer failed (exit $LASTEXITCODE)."
}

Write-Ok 'Nanna Coder installed inside WSL'
Write-Host ''
Write-Host 'Use it with:' -ForegroundColor Cyan
Write-Host "  wsl -d $Distro -- bash -lc 'podman pod ps'"
Write-Host "  wsl -d $Distro -- bash -lc 'podman logs -f harness-service'"
