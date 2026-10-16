# Windows environment

Environment installed on 2026-10-16 and used for final acceptance on 2026-10-17. `rust-toolchain.toml` selects rolling Rust stable; the versions below record this machine rather than pinning future Rust releases. `Cargo.lock` records application dependency versions.

| Component | Verified installation |
| --- | --- |
| Rustup | 1.29.1 |
| Rust / Cargo | 1.99.1, `stable-x86_64-pc-windows-msvc` |
| Rust components | cargo, rustfmt, clippy |
| Visual Studio Build Tools | 17.14.41 |
| MSVC | 14.44.35207, x64 linker |
| Windows SDK | 10.0.26100.0 |
| FFmpeg / ffprobe | 9.0.2, Gyan Essentials x64 |
| yt-dlp | 2026.08.19, official Windows standalone |
| Deno | 2.9.7, x86_64-pc-windows-msvc |

## Setup and build

Run from a PowerShell terminal in the repository:

```powershell
./scripts/setup-windows.ps1
cargo test --locked
./scripts/package-windows.ps1
```

The setup script installs Rust and MSVC/SDK components when missing, verifies the linker and SDK, and installs the media sidecars. It can be rerun; both its complete run and the `-SkipVisualStudio -SkipMediaTools` verification path passed on this machine. Cargo's bin directory is present in the persistent user PATH. Open a new terminal after first installation so PATH changes take effect.

Ordinary Cargo commands discover the installed MSVC environment. When working in a Visual Studio developer shell, use the standard x64 `VsDevCmd.bat` environment rather than temporary hand-written `LIB`/`INCLUDE` values. The installed developer command file is `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat`.

## Sidecars and cache

Development tools reside at `%APPDATA%\AnimRef\AnimRef\data\tools`. A packaged executable first checks its adjacent `tools` folder, then its own directory, then the development tools location and PATH. The release zip includes all four media executables and their license notices.

YouTube clips are cached under `%LOCALAPPDATA%\AnimRef\AnimRef\cache\youtube`. Scene files preserve the original URL and source range. Cached media is external to `.purgif`; moving a scene to another machine may require selecting and downloading its section again. Recent-scene history uses `%APPDATA%\AnimRef\AnimRef\data\recent.json`, with a first-run fallback to the original `%APPDATA%\animref\config.json` history.

The setup script verifies pinned SHA-256 values for FFmpeg and yt-dlp, the official yt-dlp checksum manifest, and Deno's publisher signature and version. Exact source URLs, versions and license texts are recorded in `tools/licenses`.

## YouTube pipeline evidence

The public 19-second sample `jNQXAC9IVRw` returned metadata without browser cookies. A request for source seconds 2–4 produced a 2.000-second H.264/AAC clip (84,892 bytes), verified with ffprobe and actual frame decoding. The application-job integration test independently verified metadata lookup, explicit section preparation, cache reuse, duration and decoding. Keyframes and media segmentation can require adjacent source data; the app does not promise exact network-byte boundaries.
