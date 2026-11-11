# Builds dist\AirLink-<ver>-windows-x64-setup.exe
# Needs: Rust (MSVC), CMake, Inno Setup 6  (winget install Kitware.CMake JRSoftware.InnoSetup)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value

$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"   # libopus's CMakeLists predates CMake 4
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# Ship the VC++ runtime app-locally so it runs on clean machines.
New-Item -ItemType Directory -Force dist\windows | Out-Null
foreach ($dll in "vcruntime140.dll", "vcruntime140_1.dll") {
    $src = Join-Path $env:SystemRoot "System32\$dll"
    if (Test-Path $src) { Copy-Item $src dist\windows\ -Force }
}

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found. Install: winget install JRSoftware.InnoSetup" }

& $iscc "/DAppVersion=$version" packaging\windows\airlink.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "Built dist\AirLink-$version-windows-x64-setup.exe"
