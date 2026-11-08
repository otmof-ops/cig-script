# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: Apache-2.0
# CigScript installer for Windows. Downloads a binary release, verifies its
# sha256 (and the gh attestation when gh is on PATH), and offers to add the
# install directory to the user's PATH. No admin rights required.
#
#   Review first (recommended):
#     irm https://raw.githubusercontent.com/otmof-ops/cig-script/v1.6.0/install.ps1 -OutFile install.ps1
#     notepad install.ps1 ; .\install.ps1
#
#   One-liner (reads CIG_VERSION, CIG_PREFIX, CIG_NO_MODIFY_PATH, CIG_YES from
#   the environment because `iex` cannot pass parameters):
#     irm https://raw.githubusercontent.com/otmof-ops/cig-script/main/install.ps1 | iex
#
#   Options:
#     -Version vX.Y.Z    install this release (default: the newest stable release)
#     -Prefix DIR        install directory (default: %LOCALAPPDATA%\Programs\cig)
#     -NoModifyPath      never touch the user's PATH
#     -Yes               answer yes to every question

param(
    [string]$Version,
    [string]$Prefix,
    [switch]$NoModifyPath,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
# The progress bar makes Invoke-WebRequest slow on Windows PowerShell 5.1.
$ProgressPreference = 'SilentlyContinue'

# Environment-variable fallbacks for `irm ... | iex`, where parameters cannot
# be passed on the command line.
if (-not $Version -and $env:CIG_VERSION) { $Version = $env:CIG_VERSION }
if (-not $Prefix) {
    if ($env:CIG_PREFIX) { $Prefix = $env:CIG_PREFIX }
    else { $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\cig' }
}
if (-not $NoModifyPath -and $env:CIG_NO_MODIFY_PATH) { $NoModifyPath = $true }
if (-not $Yes -and $env:CIG_YES) { $Yes = $true }

$Repo = 'otmof-ops/cig-script'

function Say([string]$m) { Write-Host $m }
function Fail([string]$m) {
    # `exit` inside `irm ... | iex` would close the caller's shell; a thrown
    # error stops the script and leaves the prompt alive.
    throw "install.ps1: error: $m"
}
function Ask([string]$m) {
    if ($Yes) { return $true }
    $ans = Read-Host "$m [y/N]"
    return ($ans -match '^(y|Y|yes|YES)$')
}
function Have([string]$n) { [bool](Get-Command $n -ErrorAction SilentlyContinue) }

# Force TLS 1.2+ on Windows PowerShell 5.1, where it is not the default.
try {
    [Net.ServicePointManager]::SecurityProtocol = `
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch { }

# The processor architecture, from the environment.
$archRaw = $env:PROCESSOR_ARCHITECTURE
switch ($archRaw) {
    'AMD64' { $arch = 'x86_64' }
    'ARM64' { $arch = 'aarch64' }
    default { Fail "unsupported architecture '$archRaw' (expected AMD64 or ARM64)" }
}

Say "CigScript installer: windows/$arch, prefix $Prefix"

# Resolve the release tag.
if ($Version) {
    $tag = $Version
} else {
    Say 'resolving the newest release...'
    try {
        $rel = Invoke-RestMethod -UseBasicParsing `
            -Headers @{ 'User-Agent' = 'cigscript-install' } `
            -Uri "https://api.github.com/repos/$Repo/releases/latest"
    } catch {
        Fail "could not read releases for $Repo (a private repository needs ``gh``; try -Version)"
    }
    $tag = $rel.tag_name
    if (-not $tag) { Fail 'no release tag found' }
}
if ($tag -notmatch '^v') { $tag = "v$tag" }

$asset = "cig-$tag-windows-$arch.exe"
$base = "https://github.com/$Repo/releases/download/$tag"
$work = Join-Path $env:TEMP ('cig-install-' + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $work | Out-Null

try {
    Say "downloading $asset ($tag)..."
    $assetPath = Join-Path $work $asset
    $sumPath = "$assetPath.sha256"

    # A download is tried three times. Only a 404 is the release's answer;
    # anything else (a 504, no network) says nothing about the release and
    # is never reported as if it did.
    function Get-Asset([string]$uri, [string]$out) {
        $last = $null
        for ($i = 1; $i -le 3; $i++) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $uri -OutFile $out
                return $null
            } catch {
                $last = $_
                $status = $null
                if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
                if ($status -eq 404) { return 404 }
                Start-Sleep -Seconds (2 * $i)
            }
        }
        return $last.Exception.Message
    }
    $why = Get-Asset "$base/$asset" $assetPath
    if ($why -eq 404) { Fail "no binary for windows-$arch in $tag" }
    elseif ($why) { Fail "could not download ${asset}: $why; the release may well have it, so try again" }
    $why = Get-Asset "$base/$asset.sha256" $sumPath
    if ($why -eq 404) { Fail "release ships no checksum for $asset; refusing to install" }
    elseif ($why) { Fail "could not download ${asset}.sha256: $why; refusing to install without it, so try again" }

    # The sha256 file is `<hex>  <name>`; take the first field.
    $expected = ((Get-Content -Raw $sumPath).Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $assetPath).Hash.ToLowerInvariant()
    if ($expected -ne $actual) {
        Fail "checksum mismatch for $asset (expected $expected, got $actual)"
    }
    Say 'verified: sha256 matches the published checksum (integrity, not authorship; see SECURITY.md)'

    # The build attestation names the release workflow that produced the
    # asset; gh checks it against Sigstore when it is installed.
    if (Have 'gh') {
        $null = & gh attestation verify $assetPath -R $Repo 2>&1
        if ($LASTEXITCODE -eq 0) {
            Say "attested: the build provenance names $Repo's release workflow"
        } else {
            Say 'attestation: gh could not verify the provenance (no network, an old gh, or not logged in); the checksum stands'
        }
    }

    # Install.
    if (-not (Test-Path -LiteralPath $Prefix)) {
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    }
    $target = Join-Path $Prefix 'cig.exe'
    Move-Item -LiteralPath $assetPath -Destination $target -Force
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

# PATH: the prefix goes on the user PATH as the registry holds it, so
# entries written as %VAR% stay unexpanded (reading them through .NET
# expands them, and writing them back would freeze them), and the change
# is announced to the desktop so a new terminal sees it.
function Test-UserPath([string]$dir) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    if (-not $key) { return $false }
    try {
        $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        $key.Close()
    }
    $full = [IO.Path]::GetFullPath($dir).TrimEnd('\')
    foreach ($p in ($raw -split ';' | Where-Object { $_ })) {
        $q = [Environment]::ExpandEnvironmentVariables($p)
        try { $q = [IO.Path]::GetFullPath($q).TrimEnd('\') } catch { }
        if ($q -ieq $full) { return $true }
    }
    return $false
}
function Add-UserPath([string]$dir) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if (-not $key) { $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment') }
    try {
        $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $new = if ($raw) { "$raw;$dir" } else { $dir }
        $key.SetValue('Path', $new, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    } finally {
        $key.Close()
    }
    try {
        $sig = '[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);'
        $u = Add-Type -MemberDefinition $sig -Name 'Broadcast' -Namespace 'CigInstall' -PassThru
        $out = [UIntPtr]::Zero
        $u::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$out) | Out-Null
    } catch { }
}
$already = $false
try { $already = Test-UserPath $Prefix } catch { }
if (-not $already) {
    if (-not $NoModifyPath -and (Ask "add $Prefix to your user PATH?")) {
        try {
            Add-UserPath $Prefix
            Say "added $Prefix to the user PATH; open a new terminal for the change to take effect"
        } catch {
            Say "could not add $Prefix to the user PATH ($($_.Exception.Message)); add it yourself when ready"
        }
    } else {
        Say "$Prefix is not on your PATH; add it yourself when ready"
    }
}

# Final version print.
Say ''
try {
    $cig = Join-Path $Prefix 'cig.exe'
    & $cig --version
} catch {
    Say "installed, but could not run ``cig --version`` from $Prefix"
}
