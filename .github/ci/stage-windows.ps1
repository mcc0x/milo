# Stage the Windows installer package for one tested executable: the executable under its asset
# name, the installer carrying that executable's SHA-256, and a checksum list for both.
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$Destination
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$asset = 'milo-x86_64-pc-windows-msvc.exe'

# A clean Windows has no Visual C++ runtime, so the executable must not need one.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = @(& $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe')[0]
if (-not $dumpbin) { throw 'dumpbin.exe was not found.' }
$imports = @(& $dumpbin /nologo /dependents $Executable | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '\.dll$' })
if ($LASTEXITCODE -ne 0 -or $imports.Count -eq 0) { throw "Could not list the DLLs $Executable imports." }
$runtime = @($imports | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|concrt|vccorlib|api-ms-win-crt-)' })
if ($runtime.Count -gt 0) { throw "$Executable needs a C runtime that clean Windows lacks: $($runtime -join ', ')" }
Write-Output "$Executable imports only $($imports -join ', ')."
[void](New-Item -ItemType Directory -Path $Destination -Force)
Copy-Item -LiteralPath $Executable -Destination (Join-Path $Destination $asset)
$sum = (Get-FileHash -LiteralPath (Join-Path $Destination $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
$template = [IO.File]::ReadAllText((Join-Path $root 'windows\install.ps1.in'))
if (-not $template.Contains("'@SHA256@'")) { throw 'The installer template has no checksum placeholder.' }
[IO.File]::WriteAllText((Join-Path $Destination 'install-windows.ps1'), $template.Replace("'@SHA256@'", "'$sum'"))
$lines = foreach ($name in $asset, 'install-windows.ps1') {
    '{0}  {1}' -f (Get-FileHash -LiteralPath (Join-Path $Destination $name) -Algorithm SHA256).Hash.ToLowerInvariant(), $name
}
[IO.File]::WriteAllText((Join-Path $Destination 'SHA256SUMS'), ($lines -join "`n") + "`n")
Write-Output "Staged $asset ($sum) in $Destination."
