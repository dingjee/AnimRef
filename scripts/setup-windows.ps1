[CmdletBinding()]
param(
    [switch]$SkipVisualStudio,
    [switch]$SkipMediaTools
)

$ErrorActionPreference = 'Stop'
$toolchain = 'stable-x86_64-pc-windows-msvc'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$rustup = Join-Path $cargoBin 'rustup.exe'
$stage = Join-Path $env:TEMP 'AnimRef-setup'
$tools = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'AnimRef\AnimRef\data\tools'
$winget = (Get-Command winget.exe -ErrorAction SilentlyContinue).Source

function Invoke-Checked([string]$File, [string[]]$Arguments) {
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $File $($Arguments -join ' ')"
    }
}

function Get-VersionLine([string]$File, [string[]]$Arguments) {
    $output = & $File @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) { throw "Version check failed: $File $($Arguments -join ' ')" }
    return ($output | Select-Object -First 1)
}

New-Item -ItemType Directory -Force -Path $stage, $tools | Out-Null

if (-not (Test-Path $rustup)) {
    if (-not $winget) { throw 'Rustup is missing and winget is unavailable for checksum-verified installation.' }
    Invoke-Checked $winget @('install', '--id', 'Rustlang.Rustup', '--exact', '--source', 'winget', '--silent', '--accept-source-agreements', '--accept-package-agreements')
    if (-not (Test-Path $rustup)) { throw "Rustup package did not create $rustup." }
}

Invoke-Checked $rustup @('toolchain', 'install', $toolchain, '--profile', 'minimal')
Invoke-Checked $rustup @('component', 'add', 'cargo', 'rustfmt', 'clippy', '--toolchain', $toolchain)

if (-not $SkipVisualStudio) {
    if (-not $winget) { throw 'Visual Studio Build Tools installation requires winget.' }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $activeInstaller = @()
    $vsPath = if (Test-Path $vswhere) {
        & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 Microsoft.VisualStudio.Component.Windows11SDK.26100 -property installationPath
    }
    $sdkLib = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib' -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.0\.26100(\.|$)' } | Select-Object -First 1
    $linker = if ($vsPath) {
        Get-ChildItem (Join-Path $vsPath 'VC\Tools\MSVC') -Filter link.exe -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\Hostx64\\x64\\link\.exe$' } | Select-Object -First 1
    }
    if (-not ($vsPath -and $sdkLib -and $linker)) {
        $activeInstaller = @(Get-Process vs_BuildTools, vs_setup_bootstrapper, setup -ErrorAction SilentlyContinue)
        if ($activeInstaller) {
            Write-Warning 'Visual Studio setup is already running; let it finish, then rerun this script to verify the linker and Windows SDK.'
            throw 'MSVC/Windows SDK setup is still in progress; rerun setup-windows.ps1 after the installer exits.'
        } elseif ($vsPath) {
            $setup = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\setup.exe'
            $setupArgs = 'modify --installPath "{0}" --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100 --quiet --wait --norestart' -f $vsPath
            $p = Start-Process $setup -ArgumentList $setupArgs -WindowStyle Hidden -PassThru -Wait
            if ($p.ExitCode -ne 0) { throw "Visual Studio component repair failed with exit code $($p.ExitCode)." }
        } else {
            $vsArgs = '--quiet --wait --norestart --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100'
            Invoke-Checked $winget @('install', '--id', 'Microsoft.VisualStudio.2022.BuildTools', '--exact', '--source', 'winget', '--silent', '--accept-source-agreements', '--accept-package-agreements', '--override', $vsArgs)
        }
    } else {
        Write-Host "Visual Studio Build Tools and Windows SDK already satisfy the Rust linker requirements: $vsPath"
    }
    if (-not $activeInstaller) {
        $vsPath = if (Test-Path $vswhere) {
            & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 Microsoft.VisualStudio.Component.Windows11SDK.26100 -property installationPath
        }
        $sdkLib = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib' -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^10\.0\.26100(\.|$)' } | Select-Object -First 1
        $linker = if ($vsPath) {
            Get-ChildItem (Join-Path $vsPath 'VC\Tools\MSVC') -Filter link.exe -Recurse -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match '\\Hostx64\\x64\\link\.exe$' } | Select-Object -First 1
        }
        if (-not ($vsPath -and $sdkLib -and $linker)) {
            throw 'Visual Studio Build Tools is installed but the required MSVC linker and Windows 11 SDK were not detected.'
        }
    }
}

if (-not $SkipMediaTools) {
    $ffmpegZip = Join-Path $stage 'ffmpeg-release-essentials.zip'
    $expectedFfmpegHash = '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'
    $actualFfmpegHash = if (Test-Path -LiteralPath $ffmpegZip) { (Get-FileHash $ffmpegZip -Algorithm SHA256).Hash.ToLowerInvariant() } else { '' }
    if ($actualFfmpegHash -ne $expectedFfmpegHash) {
        Invoke-WebRequest 'https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip' -OutFile $ffmpegZip
        $actualFfmpegHash = (Get-FileHash $ffmpegZip -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    if ($actualFfmpegHash -ne $expectedFfmpegHash) { throw 'FFmpeg archive SHA256 did not match the publisher checksum.' }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($ffmpegZip)
    try {
        foreach ($name in 'ffmpeg.exe', 'ffprobe.exe') {
            $entry = $archive.Entries | Where-Object { $_.FullName -match "(^|/)bin/$name$" } | Select-Object -First 1
            if (-not $entry) { throw "FFmpeg archive does not contain bin/$name." }
            $output = [IO.File]::Create((Join-Path $tools $name))
            try { $entryStream = $entry.Open(); try { $entryStream.CopyTo($output) } finally { $entryStream.Dispose() } } finally { $output.Dispose() }
        }
    } finally { $archive.Dispose() }

    $ytDlp = Join-Path $stage 'yt-dlp.exe'
    $ytSums = Join-Path $stage 'yt-dlp.SHA2-256SUMS'
    $expectedYtHash = '66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a'
    $actualYtHash = if (Test-Path -LiteralPath $ytDlp) { (Get-FileHash $ytDlp -Algorithm SHA256).Hash.ToLowerInvariant() } else { '' }
    if ($actualYtHash -ne $expectedYtHash) {
        Invoke-WebRequest 'https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp.exe' -OutFile $ytDlp
    }
    if (-not (Test-Path -LiteralPath $ytSums)) {
        Invoke-WebRequest 'https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/SHA2-256SUMS' -OutFile $ytSums
    }
    $sumLine = Get-Content $ytSums | Where-Object { $_ -match '\s+yt-dlp\.exe$' } | Select-Object -First 1
    if (-not $sumLine) { throw 'The official yt-dlp checksum manifest does not list yt-dlp.exe.' }
    $manifestYtHash = ($sumLine -split '\s+')[0].ToLowerInvariant()
    if ($manifestYtHash -ne $expectedYtHash) { throw 'The official yt-dlp checksum manifest no longer matches the pinned notice hash.' }
    $actualYtHash = (Get-FileHash $ytDlp -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualYtHash -ne $expectedYtHash) { throw 'yt-dlp SHA256 did not match its official release manifest.' }
    Copy-Item $ytDlp (Join-Path $tools 'yt-dlp.exe') -Force

    $denoZip = Join-Path $stage 'deno-x86_64-pc-windows-msvc.zip'
    $denoExtract = Join-Path $stage 'deno-extracted'
    if (-not (Test-Path -LiteralPath $denoZip)) {
        Invoke-WebRequest 'https://github.com/denoland/deno/releases/download/v2.9.7/deno-x86_64-pc-windows-msvc.zip' -OutFile $denoZip
    }
    Expand-Archive $denoZip $denoExtract -Force
    $denoExe = Join-Path $denoExtract 'deno.exe'
    $denoSignature = Get-AuthenticodeSignature $denoExe
    if ($denoSignature.Status -ne 'Valid' -or $denoSignature.SignerCertificate.Subject -notmatch 'Deno Land Inc') {
        throw 'The downloaded Deno binary did not have a valid Deno Land signature.'
    }
    $denoVersion = Get-VersionLine $denoExe @('--version')
    if ($denoVersion -notmatch '^deno 2\.9\.7 \(') { throw "Unexpected Deno version: $denoVersion" }
    Copy-Item $denoExe (Join-Path $tools 'deno.exe') -Force
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathParts = @($userPath -split ';' | Where-Object { $_ })
if ($pathParts -notcontains $tools) {
    [Environment]::SetEnvironmentVariable('Path', (($pathParts + $tools) -join ';'), 'User')
}
$env:Path = "$tools;$cargoBin;$env:Path"

Write-Host "Rust toolchain: $(Get-VersionLine (Join-Path $cargoBin 'rustc.exe') @('--version'))"
Write-Host "Cargo: $(Get-VersionLine (Join-Path $cargoBin 'cargo.exe') @('--version'))"
Write-Host "rustfmt: $(Get-VersionLine (Join-Path $cargoBin 'rustfmt.exe') @('--version'))"
Write-Host "clippy: $(Get-VersionLine (Join-Path $cargoBin 'clippy-driver.exe') @('--version'))"
Write-Host "Media tools: $tools"
foreach ($name in 'ffmpeg', 'ffprobe', 'yt-dlp', 'deno') {
    $exe = Join-Path $tools "$name.exe"
    if (Test-Path $exe) {
        $versionArgs = if ($name -eq 'ffmpeg' -or $name -eq 'ffprobe') { @('-version') } else { @('--version') }
        Write-Host "$name`: $(Get-VersionLine $exe $versionArgs)"
    }
}

if (-not $SkipVisualStudio -and $vsPath -and $linker -and $sdkLib) {
    Write-Host "Visual Studio Build Tools: $vsPath"
    Write-Host "MSVC linker: $($linker.FullName)"
    Write-Host "Windows SDK libraries: $($sdkLib.FullName)"
}
