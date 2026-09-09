<#
.SYNOPSIS
  ripbi installer for Windows.

.DESCRIPTION
  Downloads the release zip, verifies its SHA-256 against the published
  sha256sums.txt, and installs ripbi.exe into %USERPROFILE%\.local\bin as
  both ripbi.exe and its short alias rib.exe (a hard link to the same
  binary).

  Pipe directly (parameters are not passable this way; use RIPBI_VERSION):

    irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 | iex

  Or download, review, then run from the file:

    irm https://raw.githubusercontent.com/bgarcevic/ripbi/main/install.ps1 -OutFile install.ps1
    powershell -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.1.0

  The -ExecutionPolicy flag is only needed when your execution policy blocks
  downloaded script files; the piped form never touches it.

  Environment: RIPBI_VERSION pins a release ("v0.1.0" or "0.1.0"),
  RIPBI_VERBOSE=1 prints every step, RIPBI_INSTALL_DIR overrides the
  destination.
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
  [string]$Version = $env:RIPBI_VERSION,
  [switch]$Help
)

$ErrorActionPreference = 'Stop'
# Empty under `irm | iex`; in that mode a fatal error must not `exit` the caller's shell.
$piped = -not $PSScriptRoot

$Repo = 'https://github.com/bgarcevic/ripbi'
$Api = 'https://api.github.com/repos/bgarcevic/ripbi'

if ($env:RIPBI_VERBOSE -eq '1') {
  $VerbosePreference = 'Continue'
}

if ($PSVersionTable.PSVersion.Major -le 5) {
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

function Write-Info([string]$Message) {
  Write-Host "install.ps1: $Message"
}

function Show-Help {
  Write-Host @'
ripbi installer for Windows

Usage: install.ps1 [-Version VERSION] [-Help]

  -Version VERSION   install a specific release ("v0.1.0" or "0.1.0")
  -Help              show this help

Environment:
  RIPBI_VERSION      same as -Version
  RIPBI_VERBOSE=1    print every step
  RIPBI_INSTALL_DIR  install destination (default: %USERPROFILE%\.local\bin)
'@
}

try {
  if ($Help) {
    Show-Help
    return
  }

  $arch = $env:PROCESSOR_ARCHITECTURE
  if ($arch -eq 'x86' -and $env:PROCESSOR_ARCHITEW6432 -eq 'AMD64') {
    $arch = 'AMD64'
  }
  if ($arch -ne 'AMD64') {
    throw "no prebuilt ripbi for a '$arch' processor; pick an archive manually from $Repo/releases"
  }
  $target = 'x86_64-pc-windows-msvc'

  if (-not $Version) {
    Write-Info 'resolving the latest release'
    $Version = (Invoke-RestMethod -Uri "$Api/releases/latest").tag_name
  }
  $Version = $Version.TrimStart('v')
  if ($Version -notmatch '^[0-9A-Za-z.-]+$') {
    throw "'$Version' does not look like a release version"
  }

  $archiveName = "ripbi-$Version-$target.zip"
  $base = "$Repo/releases/download/v$Version"
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("ripbi-install-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmp -Force | Out-Null

  try {
    Write-Info "downloading $archiveName"
    Invoke-WebRequest -Uri "$base/$archiveName" -OutFile (Join-Path $tmp $archiveName) -UseBasicParsing
    $sumsPath = Join-Path $tmp 'sha256sums.txt'
    Invoke-WebRequest -Uri "$base/sha256sums.txt" -OutFile $sumsPath -UseBasicParsing

    Write-Info 'verifying the sha256 checksum'
    $sumLine = Select-String -Path $sumsPath -Pattern ('^([0-9a-fA-F]{64})\s+' + [regex]::Escape($archiveName) + '\s*$') |
      Select-Object -First 1
    if (-not $sumLine) {
      throw "$archiveName is not listed in sha256sums.txt"
    }
    $expected = $sumLine.Matches[0].Groups[1].Value
    $actual = (Get-FileHash -Path (Join-Path $tmp $archiveName) -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
      throw "checksum mismatch for ${archiveName}: expected $expected, got $actual"
    }
    Write-Info 'checksum ok'

    $extractDir = Join-Path $tmp 'extracted'
    Expand-Archive -Path (Join-Path $tmp $archiveName) -DestinationPath $extractDir -Force
    $exe = Join-Path (Join-Path $extractDir "ripbi-$Version") 'ripbi.exe'
    if (-not (Test-Path $exe)) {
      throw 'the archive did not contain the expected binary'
    }

    $installDir = if ($env:RIPBI_INSTALL_DIR) { $env:RIPBI_INSTALL_DIR } else { Join-Path $env:USERPROFILE '.local\bin' }
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item $exe (Join-Path $installDir 'ripbi.exe') -Force

    # Same binary, shorter name. A hard link is enough; fall back to a copy on
    # filesystems that refuse links.
    $installedExe = Join-Path $installDir 'ripbi.exe'
    $alias = Join-Path $installDir 'rib.exe'
    try {
      New-Item -ItemType HardLink -Path $alias -Value $installedExe -Force -ErrorAction Stop | Out-Null
    } catch {
      Copy-Item $installedExe $alias -Force
    }

    $pathDirs = @($env:Path -split ';' | Where-Object { $_ })
    if ($pathDirs -notcontains $installDir) {
      Write-Host ''
      Write-Host "$installDir is not on your PATH. Add it for your user with:"
      Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$installDir', 'User')"
    }
    Write-Host "installed ripbi $Version to $installedExe (also usable as $alias)"
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  }
} catch {
  Write-Error $_
  if (-not $piped) {
    exit 1
  }
}
