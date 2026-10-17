[CmdletBinding()]
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$toolsSource = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'AnimRef\AnimRef\data\tools'
$output = Join-Path $root 'dist\AnimRef'

if (-not $SkipBuild) {
    if (-not (Test-Path $cargo)) { throw "Cargo was not found at $cargo. Run scripts/setup-windows.ps1 first." }
    Push-Location $root
    try { & $cargo build --release --locked; if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" } }
    finally { Pop-Location }
}

$binary = Join-Path $root 'target\release\animref.exe'
if (-not (Test-Path $binary)) { throw "Release executable is missing: $binary" }
if (-not (Test-Path $toolsSource)) { throw "Media tools are missing at $toolsSource. Run scripts/setup-windows.ps1 first." }
foreach ($name in 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'deno.exe') {
    if (-not (Test-Path (Join-Path $toolsSource $name))) { throw "Required media tool is missing: $name" }
}
$licensesSource = Join-Path $root 'tools\licenses'
if (-not (Test-Path (Join-Path $licensesSource 'THIRD-PARTY-NOTICES.txt'))) {
    throw "Media tool licenses are missing at $licensesSource."
}

New-Item -ItemType Directory -Force -Path $output | Out-Null
Copy-Item $binary (Join-Path $output 'AnimRef.exe') -Force
$toolsOutput = Join-Path $output 'tools'
New-Item -ItemType Directory -Force -Path $toolsOutput | Out-Null
foreach ($name in 'ffmpeg.exe', 'ffprobe.exe', 'yt-dlp.exe', 'deno.exe') {
    Copy-Item (Join-Path $toolsSource $name) $toolsOutput -Force
}
$licenseOutput = Join-Path $toolsOutput 'licenses'
New-Item -ItemType Directory -Force -Path $licenseOutput | Out-Null
Copy-Item (Join-Path $licensesSource '*') $licenseOutput -Recurse -Force
$readme = Join-Path $root 'README.md'
Copy-Item $readme $output -Force
Copy-Item (Join-Path $root 'LICENSE') $output -Force
$docsOutput = Join-Path $output 'docs'
New-Item -ItemType Directory -Force -Path $docsOutput | Out-Null
Copy-Item (Join-Path $root 'docs\*') $docsOutput -Recurse -Force
$archive = Join-Path (Split-Path -Parent $output) 'AnimRef-Windows.zip'
Compress-Archive -Path (Join-Path $output '*') -DestinationPath $archive -CompressionLevel Optimal -Force
Write-Host "Packaged AnimRef to $output and $archive"
