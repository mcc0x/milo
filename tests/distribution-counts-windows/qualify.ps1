param(
    [Parameter(Mandatory)][string]$Package,
    [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExeSha256,
    [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$InstallerSha256,
    [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ManifestSha256
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted') {
    throw 'This fixture requires a disposable GitHub-hosted Windows runner.'
}
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole('Administrators')) {
    throw 'Runner admin is required only for hosts, TLS trust and firewall provisioning.'
}
$hostName = 'counts-production.up.railway.app'
$package = (Resolve-Path -LiteralPath $Package).ProviderPath
$repo = (Resolve-Path .).ProviderPath
$python = (Get-Command python.exe -CommandType Application).Source
$git = (Get-Command git.exe -CommandType Application).Source
$gitRoot = Split-Path -Parent (Split-Path -Parent $git)
$openssl = @('usr\bin\openssl.exe','mingw64\bin\openssl.exe') |
    ForEach-Object { Join-Path $gitRoot $_ } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
if (!$openssl) { throw 'Authentic Git for Windows OpenSSL not found' }
$curl = Join-Path $env:SystemRoot 'System32\curl.exe'
$curlInfo = & $curl -q --version
if ($LASTEXITCODE -ne 0 -or ($curlInfo -join ' ') -notmatch 'Schannel') { throw 'System32 curl must use Schannel' }
$names = @('milo-x86_64-pc-windows-msvc.exe', 'install-windows.ps1', 'SHA256SUMS')
$pins = @($ExeSha256.ToLowerInvariant(), $InstallerSha256.ToLowerInvariant(), $ManifestSha256.ToLowerInvariant())
for ($i = 0; $i -lt $names.Count; $i++) {
    if ((Get-FileHash -LiteralPath (Join-Path $package $names[$i])).Hash -ine $pins[$i]) { throw "Wrong sealed input: $($names[$i])" }
}
$name = 'm26c' + [Guid]::NewGuid().ToString('N').Substring(0, 12)
$work = Join-Path $env:SystemDrive "$name-qualification"
$hosts = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
$hostsOriginal = [IO.File]::ReadAllBytes($hosts)
$knownHosts = [Text.Encoding]::ASCII.GetString($hostsOriginal) -split '[\r\n]+' | ForEach-Object { ($_ -split '#',2)[0] }
if ($knownHosts -match [regex]::Escape($hostName)) { throw 'Production hostname already present in runner hosts' }
$hostsChanged = $false
$server = $null
$rootThumb = $null
$account = $null
$firewall = [System.Collections.Generic.List[string]]::new()
function CheckExternal { if ($LASTEXITCODE -ne 0) { throw "Command failed with exit $LASTEXITCODE" } }
function OpenSSL { & $openssl @args 2>&1 | Out-File -LiteralPath (Join-Path $work 'pki\openssl.log') -Append; CheckExternal }
function RunAsStandard([string]$mode, [string[]]$extra = @()) {
    $params = @{
        Credential = $account; LoadUserProfile = $true; WorkingDirectory = $work; PassThru = $true; Wait = $true
        ArgumentList = @('-E','-u','fixture.py',$mode) + $extra
        RedirectStandardOutput = Join-Path $work "$mode.out"
        RedirectStandardError = Join-Path $work "$mode.err"
    }
    $run = Start-Process $python @params
    Get-Content -LiteralPath (Join-Path $work "$mode.out"), (Join-Path $work "$mode.err")
    if ($run.ExitCode -ne 0) { throw "Standard-user $mode exited $($run.ExitCode)" }
}

try {
    $pass = ConvertTo-SecureString ("M!a1" + [Guid]::NewGuid().ToString('N')) -AsPlainText -Force
    $user = New-LocalUser -Name $name -Password $pass -AccountNeverExpires
    $account = [PSCredential]::new("$env:COMPUTERNAME\$name", $pass)
    $standard = Get-LocalGroup -SID 'S-1-5-32-545'
    if (-not (Get-LocalGroupMember $standard | Where-Object SID -eq $user.SID)) { Add-LocalGroupMember $standard -Member $user }
    [void](New-Item -ItemType Directory -Path $work)
    & icacls $work /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*$($user.SID.Value):(OI)(CI)M" | Out-Null
    CheckExternal
    $pki = Join-Path $work 'pki'
    [void](New-Item -ItemType Directory -Path $pki, (Join-Path $work 'package'))
    & icacls $pki /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    CheckExternal
    $evidence = Join-Path $work 'evidence'
    [void](New-Item -ItemType Directory -Path $evidence)
    & icacls $evidence /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "*$($user.SID.Value):(OI)(CI)RX" | Out-Null
    CheckExternal
    foreach ($file in $names) {
        $target = Join-Path $work "package\$file"
        Copy-Item -LiteralPath (Join-Path $package $file) -Destination $target
        (Get-Item -LiteralPath $target).IsReadOnly = $false
    }
    $fixture = Join-Path $work 'fixture.py'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixture.py') -Destination $fixture
    & icacls $fixture /inheritance:r /grant:r '*S-1-5-18:F' '*S-1-5-32-544:F' "*$($user.SID.Value):RX" | Out-Null
    CheckExternal
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'openssl.cnf') -Destination $pki
    Copy-Item -LiteralPath (Join-Path $repo 'tests\windows-counts\installed.py') -Destination $work
    $git | Set-Content -LiteralPath (Join-Path $work 'git.txt') -Encoding utf8

    $driver = Join-Path $repo "tests\$name.rs"
    $harness = Get-Content -Raw -LiteralPath (Join-Path $repo 'tests\windows_console.rs')
    $case = Get-Content -Raw -LiteralPath (Join-Path $repo 'tests\windows-counts\console.rs')
    try {
        [IO.File]::WriteAllText($driver, $harness + [Environment]::NewLine + $case)
        $env:CARGO_BIN_EXE_milo = Join-Path $work 'installed\milo.exe'
        & rustc +1.91.0 --edition=2024 --test $driver -o (Join-Path $work 'driver.exe')
        CheckExternal
    } finally {
        Remove-Item -LiteralPath $driver -ErrorAction SilentlyContinue
    }

    Push-Location $pki
    try {
        [void](New-Item -ItemType Directory certs)
        [IO.File]::WriteAllText((Join-Path $pki 'index.txt'), '')
        '1000' | Set-Content -LiteralPath (Join-Path $pki 'serial') -Encoding ascii
        OpenSSL req -config openssl.cnf -x509 -newkey rsa:2048 -nodes -sha256 -days 2 -extensions rootext -subj "/CN=Milo26-Offline-Issuer-$name" -keyout root.key -out root.pem
        OpenSSL req -config openssl.cnf -new -newkey rsa:2048 -nodes -subj "/CN=$hostName" -keyout leaf.key -out leaf.csr
        OpenSSL ca -config openssl.cnf -batch -extensions leafext -in leaf.csr -out leaf.pem
        OpenSSL ca -config openssl.cnf -gencrl -out crl.pem
        OpenSSL crl -in crl.pem -outform DER -out fixture.crl
        OpenSSL x509 -in root.pem -outform DER -out issuer.cer
        Remove-Item -LiteralPath root.key
    } finally { Pop-Location }

    $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore)
    if ($profiles.Count -ne 3 -or @($profiles | Where-Object { $_.Enabled -ne $true }).Count) { throw 'Expected all three Windows Firewall profiles enabled' }
    $loopback = @(Get-NetIPAddress -IPAddress '127.0.0.1' | Select-Object -ExpandProperty InterfaceIndex -Unique)
    if ($loopback.Count -ne 1) { throw 'Expected a single IPv4 loopback interface.' }
    $nonLoopback = @(Get-NetIPInterface | Where-Object InterfaceIndex -ne $loopback[0])
    $aliases = @($nonLoopback | Select-Object -ExpandProperty InterfaceAlias -Unique)
    if (!$aliases.Count -or @($aliases | Where-Object { !$_ -or $_ -match '[*?]' }).Count) { throw 'Could not enumerate concrete non-loopback interfaces.' }
    foreach ($route in Get-NetRoute -PolicyStore ActiveStore | Where-Object DestinationPrefix -in @('0.0.0.0/0','::/0')) {
        if ($route.InterfaceIndex -notin $nonLoopback.InterfaceIndex) { throw 'A default route is outside the isolated interfaces.' }
    }
    foreach ($program in @($curl, (Join-Path $work 'installed\milo.exe'))) {
        $ruleName = "$name-$([IO.Path]::GetFileNameWithoutExtension($program))-nonloopback"
        $firewall.Add($ruleName)
        $ruleParams = @{
            Name = $ruleName; DisplayName = $ruleName; Profile = 'Any'; Enabled = 'True'
            Direction = 'Outbound'; Action = 'Block'; Program = $program; InterfaceAlias = $aliases
        }
        New-NetFirewallRule @ruleParams | Out-Null
        $active = Get-NetFirewallRule -Name $ruleName -PolicyStore ActiveStore
        if ($active.Enabled -ne 'True' -or $active.Action -ne 'Block' -or $active.Direction -ne 'Outbound') { throw "Inactive denial: $ruleName" }
        if (($active | Get-NetFirewallApplicationFilter).Program -ine $program) { throw "Wrong egress path: $ruleName" }
        $denied = @($active | Get-NetFirewallInterfaceFilter | Select-Object -ExpandProperty InterfaceAlias)
        if (Compare-Object $aliases $denied) { throw "Wrong denied interfaces: $ruleName" }
    }

    $hostsChanged = $true
    $nl = [Environment]::NewLine
    [IO.File]::AppendAllText($hosts, "${nl}127.0.0.1 $hostName${nl}", [Text.Encoding]::ASCII)
    Clear-DnsClientCache
    $addresses = @([Net.Dns]::GetHostAddresses($hostName) | ForEach-Object IPAddressToString)
    if ($addresses.Count -ne 1 -or $addresses[0] -ne '127.0.0.1') { throw 'Unsafe host resolution; no candidate was run' }
    $serverParams = @{
        WorkingDirectory = $work; PassThru = $true; ArgumentList = @('-I','-u','fixture.py','serve')
        RedirectStandardOutput = Join-Path $work 'serve.out'
        RedirectStandardError = Join-Path $work 'serve.err'
    }
    $server = Start-Process $python @serverParams
    for ($i=0; $i -lt 100 -and !(Test-Path (Join-Path $work 'ready')); $i++) {
        if ($server.HasExited) { break }
        Start-Sleep -Milliseconds 100
    }
    if (!(Test-Path (Join-Path $work 'ready'))) { Get-Content (Join-Path $work 'serve.err'); throw 'TLS/CRL listener failed to start' }
    RunAsStandard negative
    $certFile = Join-Path $pki 'issuer.cer'
    $rootThumb = [Security.Cryptography.X509Certificates.X509Certificate2]::new($certFile).Thumbprint
    [void](Import-Certificate -FilePath $certFile -CertStoreLocation Cert:\LocalMachine\Root)
    RunAsStandard qualify $pins
} finally {
    if ($server -and !$server.HasExited) { Stop-Process -Id $server.Id -Force -ErrorAction Continue }
    if ($rootThumb) { Remove-Item -LiteralPath "Cert:\LocalMachine\Root\$rootThumb" -ErrorAction Continue }
    if ($hostsChanged) {
        [IO.File]::WriteAllBytes($hosts, $hostsOriginal)
        Clear-DnsClientCache
    }
    foreach ($ruleName in $firewall) { Remove-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue }
    if ($account) { Remove-LocalUser -Name $name -ErrorAction Continue }
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction Continue }
}
