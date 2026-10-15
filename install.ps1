<#
.SYNOPSIS
Build inkstone and put it where it can be run from anywhere.

.DESCRIPTION
Builds `inkstone` and `inkstone-server` (and, with -WithGui, `inkstone-gui`) in
release mode and copies them into cargo's bin directory, which is on the PATH of
anybody who installed Rust with rustup.

The two have to be installed *together*: the terminal frontend looks for the
daemon next to its own executable, so a copy of `inkstone.exe` on its own would
start nothing.

Run it again after pulling: an unchanged tree rebuilds in seconds, and only the
crates that changed are compiled, because this is the same `target` directory a
`cargo build` uses.

.PARAMETER Prefix
Where to install, instead of cargo's bin directory. The directory is created if
it is not there.

.PARAMETER WithGui
Build and install the windowed frontend as well. It pulls in wgpu, so the first
build of it takes minutes rather than seconds.

.PARAMETER AddToPath
Add the install directory to your *user* PATH if it is not on it — no admin
rights, and reversible from the environment variables dialog. Without it a
directory that is not on the PATH is only reported, with the command to add it.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File install.ps1

.EXAMPLE
powershell -ExecutionPolicy Bypass -File install.ps1 -WithGui -AddToPath
#>
[CmdletBinding()]
param(
    [string]$Prefix = "",
    [switch]$WithGui,
    [switch]$AddToPath
)

$ErrorActionPreference = "Stop"
$root = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

if (-not $Prefix) {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
    $Prefix = Join-Path $cargoHome "bin"
}
$Prefix = [System.IO.Path]::GetFullPath($Prefix)

Push-Location $root
try {
    # Only the packages that hold a binary: the windowed frontend drags in wgpu,
    # and a frontend needs the daemon built *beside* it but not as a dependency,
    # since it spawns it as a process.
    $packages = @("-p", "inkstone", "-p", "inkstone-server")
    if ($WithGui) { $packages += @("-p", "inkstone-gui") }

    Write-Host "building release: cargo build --release --locked $($packages -join ' ')"
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & cargo build --release --locked @packages
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with $LASTEXITCODE" }
    $watch.Stop()
    Write-Host ("built in {0:N1} s" -f $watch.Elapsed.TotalSeconds)

    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    $names = @("inkstone", "inkstone-server")
    if ($WithGui) { $names += "inkstone-gui" }
    foreach ($name in $names) {
        $from = Join-Path $root "target\release\$name.exe"
        if (-not (Test-Path $from)) { throw "the build left no $from" }
        Copy-Item -Force $from (Join-Path $Prefix "$name.exe")
        Write-Host "installed $name.exe"
    }

    # A frontend with no daemon beside it is the one mistake this can make, so
    # it is checked rather than assumed.
    $frontends = @("inkstone", "inkstone-gui") | Where-Object { $names -contains $_ }
    foreach ($frontend in $frontends) {
        if (-not (Test-Path (Join-Path $Prefix "inkstone-server.exe"))) {
            throw "$frontend.exe needs inkstone-server.exe in the same directory"
        }
    }

    Write-Host ""
    Write-Host "checking the install..."
    & (Join-Path $Prefix "inkstone.exe") --version

    # Is the directory reachable from anywhere? PATH is what decides that.
    $seen = @()
    foreach ($scope in @("User", "Machine")) {
        $value = [Environment]::GetEnvironmentVariable("Path", $scope)
        if ($value) { $seen += ($value -split ";" | Where-Object { $_ }) }
    }
    $onPath = $seen | Where-Object { $_.TrimEnd("\") -ieq $Prefix.TrimEnd("\") }
    if ($onPath) {
        Write-Host "`n$Prefix is on the PATH already."
    } elseif ($AddToPath) {
        $value = [Environment]::GetEnvironmentVariable("Path", "User")
        $value = if ($value) { "$value;$Prefix" } else { $Prefix }
        [Environment]::SetEnvironmentVariable("Path", $value, "User")
        Write-Host "`nadded $Prefix to your user PATH — open a new terminal for it."
    } else {
        Write-Host "`n$Prefix is not on the PATH. Either run it by full path, or add it with:"
        Write-Host "    [Environment]::SetEnvironmentVariable('Path', ([Environment]::GetEnvironmentVariable('Path', 'User')) + ';$Prefix', 'User')"
        Write-Host "or re-run this script with -AddToPath."
    }

    Write-Host "`nTry:  inkstone some-file.txt"
    Write-Host "Update:  git pull; powershell -ExecutionPolicy Bypass -File install.ps1"
} finally {
    Pop-Location
}
