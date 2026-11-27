# Windows PowerShell installer for office_mode
# Usage:
#   irm https://raw.githubusercontent.com/Rossonerian/office_mode/main/scripts/install.ps1 | iex
# Or with parameters:
#   .\install.ps1 -Version v1.0.0
#   .\install.ps1 -Uninstall

[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$InstallDir = "$env:LOCALAPPDATA\office_mode",
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = "Rossonerian/office_mode"
$Target = "x86_64-pc-windows-msvc"

if ($Uninstall) {
    Write-Host "Removing office_mode from $InstallDir ..."
    if (Test-Path $InstallDir) {
        Remove-Item -Recurse -Force $InstallDir
    }
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath) {
        $Cleaned = ($UserPath -split ";" | Where-Object { $_ -and $_ -ne $InstallDir }) -join ";"
        if ($Cleaned -ne $UserPath) {
            [Environment]::SetEnvironmentVariable("Path", $Cleaned, "User")
            Write-Host "Removed $InstallDir from User PATH."
        }
    }
    Write-Host "Done."
    exit 0
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $ReleaseInfo = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "office_mode-installer" }
    $Version = $ReleaseInfo.tag_name
    if ([string]::IsNullOrWhiteSpace($Version)) {
        throw "Could not determine latest release tag from GitHub."
    }
}

$Archive = "office_mode-$Version-$Target.zip"
$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("office_mode_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

try {
    $ZipPath = Join-Path $TempDir $Archive
    $SumsPath = Join-Path $TempDir "SHA256SUMS.txt"

    Write-Host "Downloading $Archive ($Version)..."
    Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ZipPath -UseBasicParsing
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS.txt" -OutFile $SumsPath -UseBasicParsing

    $ExpectedLine = Get-Content $SumsPath | Where-Object { $_ -match "\s$([regex]::Escape($Archive))$" } | Select-Object -First 1
    if (-not $ExpectedLine) {
        throw "Checksum entry for $Archive not found in SHA256SUMS.txt"
    }
    $ExpectedHash = ($ExpectedLine -split "\s+")[0].ToUpperInvariant()
    $ActualHash = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($ExpectedHash -ne $ActualHash) {
        throw "SHA-256 mismatch for $Archive (expected $ExpectedHash, got $ActualHash)"
    }
    Write-Host "Checksum verified ($ActualHash)."

    $UnpackDir = Join-Path $TempDir "unpack"
    Expand-Archive -Path $ZipPath -DestinationPath $UnpackDir -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path (Join-Path $UnpackDir "office.exe") -Destination (Join-Path $InstallDir "office.exe") -Force
    Copy-Item -Path (Join-Path $UnpackDir "office-hook.exe") -Destination (Join-Path $InstallDir "office-hook.exe") -Force
    if (Test-Path (Join-Path $UnpackDir "office-desktop.exe")) {
        Copy-Item -Path (Join-Path $UnpackDir "office-desktop.exe") -Destination (Join-Path $InstallDir "office-desktop.exe") -Force
    }

    $DestAssets = Join-Path $InstallDir "assets"
    if (Test-Path $DestAssets) {
        Remove-Item -Recurse -Force $DestAssets
    }
    Copy-Item -Path (Join-Path $UnpackDir "assets") -Destination $DestAssets -Recurse -Force

    # Convenience alias wrapper so both `office` and `office_mode` work in PowerShell / cmd
    $Shim = "@echo off`r`n`"%~dp0office.exe`" %*`r`n"
    Set-Content -Path (Join-Path $InstallDir "office_mode.cmd") -Value $Shim -Encoding Ascii

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $Segments = @()
    if ($UserPath) {
        $Segments = $UserPath -split ";" | Where-Object { $_ }
    }
    if ($Segments -notcontains $InstallDir) {
        $NewPath = ($Segments + $InstallDir) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        $env:PATH = "$InstallDir;$env:PATH"
        Write-Host "Added $InstallDir to User PATH."
    }

    Write-Host ""
    Write-Host "Installed office_mode ($Version) to $InstallDir"
    Write-Host "Next steps:"
    Write-Host "  office install-hooks   # wire Claude Code + Antigravity hooks"
    Write-Host "  office doctor          # check setup & bundled themes"
    Write-Host "  office desktop --demo  # preview native desktop app (or 'office-desktop')"
    Write-Host "  office demo            # preview terminal office with synthetic agents"
    Write-Host "  office                 # launch live terminal office (or 'office desktop')"
}
finally {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
    }
}
