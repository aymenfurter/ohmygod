$ErrorActionPreference = 'Stop'
$version = (python scripts/check-version.py).Trim()
$installer = (Resolve-Path "dist/OH-MY-GOD_${version}_windows-x64.exe").Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ohmygod-install-" + [guid]::NewGuid())
$install = Join-Path $root 'App With Spaces!'
$data = Join-Path $root 'board-data'
New-Item -ItemType Directory -Path $data -Force | Out-Null
$env:OHMYGOD_HOME = $data
[System.IO.File]::WriteAllText((Join-Path $data 'settings.json'), '{"port":47822,"setup_completed":false}')
$app = $null
try {
    $result = Start-Process -FilePath $installer -ArgumentList "/S /D=$install" -Wait -PassThru
    if ($result.ExitCode -ne 0) { throw "Installer failed: $($result.ExitCode)" }
    $cli = Join-Path $install 'ohmygod.exe'
    $actual = (& $cli --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $actual -ne "ohmygod $version") { throw "Installed CLI version mismatch: $actual" }
    $app = Start-Process -FilePath (Join-Path $install 'ohmygod-app.exe') -PassThru
    $ready = $false
    for ($i = 0; $i -lt 60; $i++) {
        if ($app.HasExited) { throw "Installed desktop app exited: $($app.ExitCode)" }
        try {
            $health = Invoke-RestMethod 'http://127.0.0.1:47822/api/health' -TimeoutSec 1
            if ($health.name -eq 'ohmygod' -and $health.version -eq $version) { $ready = $true; break }
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }
    if (-not $ready) { throw 'Installed app did not start its board service.' }
    $joined = & $cli join --name WindowsInstallerTest --harness CI --session installer
    if ($LASTEXITCODE -ne 0 -or "$joined" -notmatch 'A1') { throw "Installed CLI cannot join: $joined" }
    $posted = 'Windows installer transport works.' | & $cli post --agent A1 --title 'Installer smoke test' --tag community
    if ($LASTEXITCODE -ne 0 -or "$posted" -notmatch 'T1') { throw "Installed CLI cannot post: $posted" }
    $read = & $cli read T1 --agent A1
    if ($LASTEXITCODE -ne 0 -or "$read" -notmatch 'Windows installer transport works') { throw "Installed CLI cannot read: $read" }
    Write-Output "Installed app and console CLI work together at version $version."
} finally {
    if ($null -ne $app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force }
    $uninstaller = Join-Path $install 'uninstall.exe'
    if (Test-Path -LiteralPath $uninstaller) {
        $result = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
        if ($result.ExitCode -ne 0) { throw "Uninstaller failed: $($result.ExitCode)" }
    }
}
