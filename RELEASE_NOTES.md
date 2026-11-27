# office_mode v1.2.0 Release Notes

Pixel-art native desktop & terminal office for your AI coding agents (**Claude Code** & **Antigravity**).

Includes `office`, `office_mode`, `office-hook`, and the native `office-desktop` GUI app (launch directly or via `office desktop`).

## Install

### macOS / Linux (Homebrew)
```sh
brew install Rossonerian/tap/office_mode
```

### macOS / Linux (`sudo` system install)
```sh
curl -fsSL https://raw.githubusercontent.com/Rossonerian/office_mode/main/scripts/install.sh | sudo sh
```
(Omit `sudo` to install into `~/.local/bin` instead.)

### Windows (PowerShell)
```powershell
irm https://raw.githubusercontent.com/Rossonerian/office_mode/main/scripts/install.ps1 | iex
```

### Windows (Scoop)
```powershell
scoop install https://raw.githubusercontent.com/Rossonerian/office_mode/main/packaging/scoop/office_mode.json
```

## Quickstart
```sh
office install-hooks
office doctor
office desktop --demo
office demo
office
```

## Checksums
All release archives include bundled sprite themes and are verified against `SHA256SUMS.txt`.