# Install, upgrade and remove Milo with the packaged installer, as a person would, in a scratch
# folder. The user PATH is changed as the installer changes it, and put back afterwards.
param(
    [Parameter(Mandatory)][string]$Release,
    [Parameter(Mandatory)][string]$Upgrade
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$work = Join-Path ([IO.Path]::GetTempPath()) ('milo-install-' + [Guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $work)
$bin = Join-Path $work "Programs $([char]0xE9)\Milo"
$env:MILO_BIN_DIR = $bin
$env:APPDATA = Join-Path $work 'roaming'
$env:LOCALAPPDATA = Join-Path $work 'local'
$binary = Join-Path $bin 'milo.exe'
$receipt = Join-Path $bin '.milo-install-receipt'
$uninstaller = Join-Path $bin 'uninstall-milo.ps1'
$failures = 0

function Check([bool]$Condition, [string]$What) {
    if ($Condition) { Write-Output "ok: $What" } else { Write-Output "FAILED: $What"; $script:failures++ }
}

function Get-RawPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try { $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } finally { $key.Close() }
}

function Invoke-Installer([string]$Script, [string[]]$Arguments = @()) {
    $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script @Arguments 2>&1 | Out-String
    [pscustomobject]@{ Code = $LASTEXITCODE; Output = $output }
}

function Digest([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

function Old-Copies { @(Get-ChildItem -LiteralPath $bin -Force -Filter '.milo-old-*.exe' -File -ErrorAction SilentlyContinue) }

$originalPath = Get-RawPath
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
$originalKind = if ($null -ne $originalPath) { $key.GetValueKind('Path') } else { $null }
$key.Close()
$running = $null
try {
    $releaseDigest = Digest (Join-Path $Release 'milo-x86_64-pc-windows-msvc.exe')
    $upgradeDigest = Digest (Join-Path $Upgrade 'milo-x86_64-pc-windows-msvc.exe')
    Check ($releaseDigest -ne $upgradeDigest) 'the two packages hold different executables'

    $tampered = Join-Path $work 'tampered'
    Copy-Item -LiteralPath $Release -Destination $tampered -Recurse
    [IO.File]::AppendAllText((Join-Path $tampered 'milo-x86_64-pc-windows-msvc.exe'), 'x')
    $result = Invoke-Installer (Join-Path $tampered 'install-windows.ps1')
    Check ($result.Code -ne 0 -and $result.Output -match 'SHA-256 mismatch') 'a package whose executable changed is refused'
    Check (-not (Test-Path -LiteralPath $binary)) 'nothing is installed from it'

    $result = Invoke-Installer (Join-Path $Release 'install-windows.ps1')
    Check ($result.Code -eq 0) "the release installs: $($result.Output.Trim())"
    Check ((Digest $binary) -eq $releaseDigest) 'the installed executable is the one tested'
    $lines = [IO.File]::ReadAllText($receipt) -split "`n"
    Check ($lines[0] -eq $releaseDigest -and $lines[1] -eq $binary -and $lines[2] -eq $bin) 'the receipt names the digest, the file and the PATH entry added'
    Check (@((Get-RawPath) -split ';' | Where-Object { $_ -eq $bin }).Count -eq 1) 'the folder is on the user PATH once'
    Check ((Test-Path -LiteralPath $uninstaller) -and ((Get-Content -LiteralPath $uninstaller -Raw) -match $releaseDigest)) 'the uninstaller is kept beside milo.exe'
    & $binary --version | Out-Null
    Check ($LASTEXITCODE -eq 0) 'the installed milo runs'

    $result = Invoke-Installer (Join-Path $Release 'install-windows.ps1')
    Check ($result.Code -eq 0) 'installing the same version again succeeds'
    Check (@((Get-RawPath) -split ';' | Where-Object { $_ -eq $bin }).Count -eq 1) 'a second install adds no second PATH entry'

    $log = Join-Path $work 'web.out'
    $running = Start-Process -FilePath $binary -ArgumentList 'web', '--no-open', '--port', '0' -PassThru -WindowStyle Hidden -RedirectStandardOutput $log
    $deadline = (Get-Date).AddSeconds(30)
    while (-not ((Test-Path -LiteralPath $log) -and (Get-Content -LiteralPath $log -Raw) -match 'http://127') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    Check (-not $running.HasExited) 'the installed milo keeps running'

    $result = Invoke-Installer $uninstaller
    Check ($result.Code -ne 0 -and $result.Output -match 'still running') 'removing a running milo is refused with a reason'
    Check ((Test-Path -LiteralPath $binary) -and (Test-Path -LiteralPath $receipt)) 'nothing is removed while it runs'

    $result = Invoke-Installer (Join-Path $Upgrade 'install-windows.ps1')
    Check ($result.Code -eq 0) "an upgrade succeeds while the old version runs: $($result.Output.Trim())"
    Check ((Digest $binary) -eq $upgradeDigest) 'the new version is in place'
    Check (@(Old-Copies).Count -eq 1) 'the running old version stepped aside'
    Check ((([IO.File]::ReadAllText($receipt)) -split "`n")[0] -eq $upgradeDigest) 'the receipt follows the upgrade'

    Stop-Process -Id $running.Id -Force
    $running.WaitForExit()
    $running = $null
    $result = Invoke-Installer (Join-Path $Upgrade 'install-windows.ps1')
    Check ($result.Code -eq 0 -and @(Old-Copies).Count -eq 0) 'the old version is cleared once it has stopped'

    $installed = [IO.File]::ReadAllBytes($binary)
    [IO.File]::AppendAllText($binary, 'x')
    $result = Invoke-Installer (Join-Path $Release 'install-windows.ps1')
    Check ($result.Code -ne 0 -and $result.Output -match 'no matching installer receipt') 'an install over a changed milo.exe is refused'
    $result = Invoke-Installer $uninstaller
    Check ($result.Code -ne 0 -and (Test-Path -LiteralPath $binary)) 'a changed milo.exe is not removed'
    [IO.File]::WriteAllBytes($binary, $installed)

    $result = Invoke-Installer $uninstaller
    Check ($result.Code -eq 0) "the uninstaller removes Milo: $($result.Output.Trim())"
    Check (@(Get-ChildItem -LiteralPath $bin -Force).Count -eq 0) 'nothing is left in the install folder'
    Check ((Get-RawPath) -eq $originalPath) 'the user PATH is as it was before'
    $result = Invoke-Installer (Join-Path $Release 'install-windows.ps1') @('-Uninstall')
    Check ($result.Code -eq 0) 'removing again is harmless'
} finally {
    if ($running -and -not $running.HasExited) { Stop-Process -Id $running.Id -Force }
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    if ($null -eq $originalPath) { $key.DeleteValue('Path', $false) } else { $key.SetValue('Path', $originalPath, $originalKind) }
    $key.Close()
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
if ($failures -gt 0) { Write-Output "$failures check(s) failed."; exit 1 }
Write-Output 'Every installer check passed.'
