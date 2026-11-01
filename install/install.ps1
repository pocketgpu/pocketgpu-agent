# PocketGPU agent installer for Windows 10 and 11:
#   irm https://pocketgpu.app/install.ps1 | iex
#
# Installs pocketgpu.exe (and the `pgpu` alias) into %LOCALAPPDATA%\PocketGPU\bin, adds it to
# your PATH and starts the agent at login, then offers the tray app and pairing a phone.
# No Administrator. Settings, all optional:
#   $env:PGPU_VERSION = 'v1.2.0'      a release tag instead of the latest
#   $env:PGPU_INSTALL_DIR = 'C:\path' instead of %LOCALAPPDATA%\PocketGPU\bin
#   $env:PGPU_NO_SERVICE = '1'        install only; don't start the agent
#   $env:PGPU_NO_TRAY = '1'           don't offer the tray app (servers, scripts)
#   $env:PGPU_REPO = 'owner/name'     GitHub repository with the releases
#   $env:PGPU_DOWNLOAD_URL = 'url'    a mirror holding the release files

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 may not offer TLS 1.2, which GitHub requires.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# Windows Terminal draws check marks; the classic console font has none.
$script:Fancy = [bool]($env:WT_SESSION -or $env:TERM_PROGRAM)
$script:Tick = if ($script:Fancy) { [string][char]0x2713 } else { '+' }
$script:Cross = if ($script:Fancy) { [string][char]0x2717 } else { 'x' }
$script:Steps = 5
$script:Step = 0

# Run through `irm ... | iex` there is no script file, and `exit` would close the person's
# PowerShell window with the message on it. Stop with `break` then, as Scoop's installer does:
# no Fail call sits inside a loop, so it ends the install and leaves the window open.
$script:FromIex = $null -eq $MyInvocation.MyCommand.Path

function Fail($message) {
    Write-Host ''
    Write-Host "  $script:Cross $message" -ForegroundColor Red
    Write-Host ''
    if ($script:FromIex) { break }
    exit 1
}

function Show-Banner {
    Write-Host ''
    Write-Host '  ' -NoNewline
    Write-Host ' >_ ' -ForegroundColor Black -BackgroundColor Green -NoNewline
    Write-Host '  PocketGPU' -ForegroundColor White
    Write-Host '        Your GPU machine, in your pocket' -ForegroundColor DarkGray
    Write-Host ''
}

# `  [2/5] Downloading the agent ....... `, followed by Done or a progress bar.
function Start-Step($text) {
    $script:Step++
    Write-Host "  [$script:Step/$script:Steps] " -ForegroundColor DarkGray -NoNewline
    Write-Host (($text + ' ').PadRight(38, '.') + ' ') -NoNewline
}

function Done($detail) {
    Write-Host $script:Tick -ForegroundColor Green -NoNewline
    if ($detail) { Write-Host " $detail" -ForegroundColor DarkGray } else { Write-Host '' }
}

function Show-Bar($done, $total) {
    $width = 30
    $full = [string][char]0x2588
    $empty = [string][char]0x2591
    if ($total -gt 0) {
        $fill = [int][Math]::Floor($width * $done / $total)
        $bar = ($full * $fill) + ($empty * ($width - $fill))
        $text = '        {0} {1,3}%  {2:N1} / {3:N1} MB' -f $bar, [int](100 * $done / $total), ($done / 1MB), ($total / 1MB)
    } else {
        $text = '        {0:N1} MB' -f ($done / 1MB)
    }
    Write-Host "`r$text" -ForegroundColor Green -NoNewline
}

# Downloads with a progress bar (Invoke-WebRequest's own is slow in Windows PowerShell).
function Save-File($url, $path, [switch]$Quiet) {
    try {
        $req = [Net.HttpWebRequest]::Create($url)
        $req.UserAgent = 'pocketgpu-installer'
        $res = $req.GetResponse()
    } catch {
        $inner = $_.Exception.InnerException
        $web = if ($inner -is [Net.WebException]) { $inner } else { $_.Exception }
        $code = if ($web.Response) { [int]$web.Response.StatusCode } else { 0 }
        if ($code -eq 404) {
            Fail "not found: $url`n    Is the version right? Releases: https://github.com/$script:Repo/releases"
        }
        Fail "could not download $url`n    $($web.Message)"
    }
    try {
        $total = $res.ContentLength
        $in = $res.GetResponseStream()
        $out = [IO.File]::Create($path)
        try {
            $buf = New-Object byte[] 65536
            $done = 0
            $shown = [DateTime]::MinValue
            while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                $done += $n
                if (-not $Quiet -and ((Get-Date) - $shown).TotalMilliseconds -ge 80) {
                    Show-Bar $done $total
                    $shown = Get-Date
                }
            }
            if (-not $Quiet) { Show-Bar $done $total; Write-Host '' }
        } finally {
            $out.Dispose()
            $in.Dispose()
        }
    } finally {
        $res.Dispose()
    }
}

# Checks a downloaded file against SHA256SUMS.
function Test-Checksum($tmp, $file) {
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') |
        Where-Object { $_ -match "\s\*?$([regex]::Escape($file))$" } | Select-Object -First 1
    if (-not $line) { Fail "SHA256SUMS has no entry for $file" }
    $want = ($line -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $file)).Hash.ToLower()
    if ($want -ne $got) { Fail "checksum mismatch for $file; the download is damaged or was changed" }
}

function Ask($question) {
    if (-not [Environment]::UserInteractive) { return $false }
    Write-Host ''
    Write-Host '  ? ' -ForegroundColor Green -NoNewline
    $answer = Read-Host "$question [Y/n]"
    return -not ($answer -match '^[nN]')
}

# The tray app and agent window, from the same release. Its installer carries the agent too,
# but leaves this one alone since it is as new.
function Install-Tray($base, $tmp) {
    $app = Join-Path $env:LOCALAPPDATA 'PocketGPU Agent'
    if (-not (Ask 'Also install the tray app (status, pairing and settings in a window)?')) { return }
    Write-Host ''
    $setup = 'PocketGPU-Agent-x86_64-setup.exe'
    Start-Step 'Downloading the tray app'
    Write-Host ''
    Save-File "$base/$setup" (Join-Path $tmp $setup)
    Test-Checksum $tmp $setup
    Start-Step 'Installing the tray app'
    Start-Process -FilePath (Join-Path $tmp $setup) -ArgumentList '/S' -Wait
    $exe = Get-ChildItem $app -Filter *.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notlike 'uninstall*' } | Select-Object -First 1
    if ($exe) {
        Start-Process $exe.FullName
        Done 'running next to the clock'
    } else {
        Done 'start "PocketGPU Agent" from the Start menu'
    }
}

function Install-PocketGPU {
    Show-Banner
    $script:Repo = if ($env:PGPU_REPO) { $env:PGPU_REPO } else { 'pocketgpu/pocketgpu-releases' }
    $dir = if ($env:PGPU_INSTALL_DIR) { $env:PGPU_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'PocketGPU\bin' }
    $base = if ($env:PGPU_DOWNLOAD_URL) { $env:PGPU_DOWNLOAD_URL }
        elseif ($env:PGPU_VERSION) { "https://github.com/$script:Repo/releases/download/$($env:PGPU_VERSION)" }
        else { "https://github.com/$script:Repo/releases/latest/download" }
    $file = 'pocketgpu-x86_64-windows.tar.gz'
    $trayOffer = -not $env:PGPU_NO_TRAY -and [Environment]::UserInteractive -and
        -not (Test-Path (Join-Path $env:LOCALAPPDATA 'PocketGPU Agent'))
    if ($trayOffer) { $script:Steps = 7 }

    Start-Step 'Checking this PC'
    if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        Fail 'PocketGPU for Windows runs on 64-bit Intel and AMD CPUs'
    }
    if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
        Fail 'tar.exe is missing; PocketGPU needs Windows 10 version 1803 or newer'
    }
    $os = (Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue).Caption -replace '^Microsoft ', ''
    if (-not $os) { $os = 'Windows' }
    # With UAC off every program has admin rights; the agent drops them itself.
    $admin = ''
    try {
        $identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
        if ($identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { $admin = ', drops admin rights' }
    } catch { }
    Done "$os, 64-bit$admin"

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('pocketgpu-' + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $label = if ($env:PGPU_VERSION) { $env:PGPU_VERSION } else { 'latest' }
        Start-Step "Downloading the agent ($label)"
        Write-Host ''
        Save-File "$base/SHA256SUMS" (Join-Path $tmp 'SHA256SUMS') -Quiet
        Save-File "$base/$file" (Join-Path $tmp $file)

        Start-Step 'Verifying the download'
        Test-Checksum $tmp $file
        # The signature on SHA256SUMS is checked by `pgpu update`; PowerShell has no minisign.
        Done 'SHA-256 matches the release'

        Start-Step 'Installing'
        tar.exe -xzf (Join-Path $tmp $file) -C $tmp
        if (-not (Test-Path (Join-Path $tmp 'pocketgpu.exe'))) { Fail 'the download has no pocketgpu.exe' }
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        # A folder outside the user's profile (C:\tools, say) inherits rights that let other
        # users change files in it: keep it to this user, SYSTEM and Administrators.
        if (-not $dir.StartsWith($env:USERPROFILE, [StringComparison]::OrdinalIgnoreCase)) {
            & icacls.exe $dir /inheritance:r /grant:r "${env:USERNAME}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
        }
        $exe = Join-Path $dir 'pocketgpu.exe'
        # A running agent keeps its file open; Windows lets it be renamed, not overwritten.
        if (Test-Path $exe) {
            Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $dir 'pocketgpu.old.exe')
            Rename-Item $exe 'pocketgpu.old.exe'
        }
        Copy-Item (Join-Path $tmp 'pocketgpu.exe') $exe
        Set-Content -Path (Join-Path $dir 'pgpu.cmd') -Value "@`"%~dp0pocketgpu.exe`" %*" -Encoding ASCII
        $version = ((& $exe --version) -split ' ')[-1]
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $pathNote = ''
        if (-not (($userPath -split ';') -contains $dir)) {
            $newPath = if ($userPath) { "$userPath;$dir" } else { $dir }
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            $pathNote = ', added to PATH'
        }
        $env:Path = "$env:Path;$dir"
        Done "pgpu $version$pathNote"

        Start-Step 'Starting the agent'
        if ($env:PGPU_NO_SERVICE) {
            Done 'skipped (PGPU_NO_SERVICE)'
        } else {
            # Restart an agent that was already running, so it uses the new version.
            & $exe service uninstall --json | Out-Null
            # Not through a pipe: agents before 0.1.0-rc.4 hand it to the background agent,
            # which keeps it open, so reading it would never end.
            $p = Start-Process -FilePath $exe -ArgumentList 'service', 'install' -WindowStyle Hidden -PassThru
            $p.WaitForExit()
            $state = (& $exe service status --json | ConvertFrom-Json).state
            if ($state -ne 'active') { Fail "the agent did not start ($state). Check with: pgpu doctor" }
            Done 'running, and starts at login'
        }

        if ($trayOffer) { Install-Tray $base $tmp }
    }
    finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
    }

    Write-Host ''
    Write-Host "  $script:Tick PocketGPU $version is installed." -ForegroundColor Green

    $paired = 1
    if (-not $env:PGPU_NO_SERVICE) {
        try { $paired = (& $exe status --json | ConvertFrom-Json).connection.paired } catch { $paired = 1 }
    }
    if ($paired -eq 0 -and (Ask 'Pair your phone now?')) {
        Write-Host ''
        & $exe pair
        return
    }
    Write-Host ''
    Write-Host '  Next' -ForegroundColor White
    $next = @()
    if ($paired -eq 0) { $next += , @('pgpu pair', 'connect your phone') }
    $next += , @('pgpu status', 'what this machine shares')
    $next += , @('pgpu run -- <command>', 'watch a job from your phone')
    foreach ($n in $next) {
        Write-Host ('    ' + $n[0].PadRight(24)) -ForegroundColor Green -NoNewline
        Write-Host $n[1] -ForegroundColor DarkGray
    }
    if ($pathNote) {
        Write-Host ''
        Write-Host '  Open a new terminal to use pgpu.' -ForegroundColor DarkGray
    }
    Write-Host ''
}

Install-PocketGPU
