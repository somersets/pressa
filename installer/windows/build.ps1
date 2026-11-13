# Builds Pressa in release mode and packages it into target\installer\Pressa-Setup-<version>.exe.
# Requires Inno Setup 6 (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    & "$PSScriptRoot\fetch-ffmpeg.ps1"

    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value

    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup' }

    & $iscc "/DAppVersion=$version" "$PSScriptRoot\pressa.iss"
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }
    Get-ChildItem target\installer\*.exe | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } }
}
finally {
    Pop-Location
}
