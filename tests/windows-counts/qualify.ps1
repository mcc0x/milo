# Builds and stages a loopback-only candidate. Only run on a disposable hosted runner:
# the installer and the entire test run as a fresh, non-administrator Windows user.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'This fixture needs a disposable GitHub-hosted Windows runner.'
}
$name = 'milo' + [Guid]::NewGuid().ToString('N').Substring(0, 12)
$work = Join-Path $env:SystemDrive "$name-qualification"
$password = ConvertTo-SecureString ("M!a1" + [Guid]::NewGuid().ToString('N')) -AsPlainText -Force
$user = New-LocalUser -Name $name -Password $password -AccountNeverExpires
try {
    $users = Get-LocalGroup -SID 'S-1-5-32-545'
    if (-not (Get-LocalGroupMember $users | Where-Object SID -eq $user.SID)) {
        Add-LocalGroupMember $users -Member $user
    }
    $null = New-Item -Type Directory -Path $work
    icacls $work /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*$($user.SID.Value):(OI)(CI)F"
    if ($LASTEXITCODE -ne 0) { throw 'Could not isolate the test directory.' }

    git rev-parse HEAD
    cargo +1.91.0 test --locked --bin milo telemetry::
    if ($LASTEXITCODE -ne 0) { throw 'Native telemetry unit tests failed.' }
    $env:MILO_COUNTS_ENDPOINT = 'http://127.0.0.1:36129/v1/counts'
    cargo +1.91.0 build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    & ./.github/ci/stage-windows.ps1 -Executable 'target\release\milo.exe' -Destination "$work\package"
    Copy-Item hosted\counts "$work\counts" -Recurse
    Copy-Item tests\windows-counts\installed.py $work
    Copy-Item (Get-Command bun.exe).Source "$work\bun.exe"
    $pg = Get-ChildItem "$env:ProgramFiles\PostgreSQL\*\bin\initdb.exe" | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $pg) { throw 'Postgres is missing from the hosted runner.' }
    $git = (Get-Command git.exe).Source
    @{ pg = $pg.DirectoryName; git = $git } | ConvertTo-Json | Set-Content "$work\tools.json" -Encoding utf8NoBOM

    $env:CARGO_BIN_EXE_milo = "$work\installed\milo.exe"
    $harness = Get-Content -Raw -LiteralPath tests\windows_console.rs
    $case = Get-Content -Raw -LiteralPath tests\windows-counts\console.rs
    $driver = Join-Path (Resolve-Path tests) "$name.rs"
    try {
        [IO.File]::WriteAllText($driver, "$harness`n$case")
        rustc +1.91.0 --edition=2024 --test $driver -o "$work\driver.exe"
        if ($LASTEXITCODE -ne 0) { throw 'Could not build the ConPTY driver.' }
    } finally {
        Remove-Item -LiteralPath $driver
    }

    $python = (Get-Command python.exe).Source
    $account = [PSCredential]::new("$env:COMPUTERNAME\$name", $password)
    $process = Start-Process $python -Credential $account -LoadUserProfile -WorkingDirectory $work -PassThru -Wait `
        -ArgumentList '-u', 'installed.py' -RedirectStandardOutput "$work\out.txt" -RedirectStandardError "$work\err.txt"
    Get-Content "$work\out.txt", "$work\err.txt"
    if ($process.ExitCode -ne 0) { throw "Installed qualification exited with $($process.ExitCode)." }
} finally {
    Remove-LocalUser -SID $user.SID
    if (Test-Path $work) { Remove-Item -Recurse -Force -LiteralPath $work }
}
