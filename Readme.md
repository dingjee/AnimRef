# AnimRef

AnimRef is a Windows reference canvas for images, animated GIFs, text, video, and YouTube sections. The native Rust/egui app reads existing `.purgif` scenes, resolves relative media references, and saves playback position, loop ranges, and grid settings.

## Build and run

Install the Windows toolchain and media sidecars, then run:

```powershell
./scripts/setup-windows.ps1
cargo run
```

The setup script installs the rolling Rust stable channel with `rustfmt` and `clippy`, Visual Studio 2022 C++ Build Tools and the Windows SDK when needed, plus FFmpeg/ffprobe, yt-dlp, and Deno. Node.js is not required. Rust stable is intentionally not pinned to a dated release; `Cargo.lock` pins application dependencies.

Create a portable release folder and zip with:

```powershell
./scripts/package-windows.ps1
```

The release is Windows 10/11 x64 and uses the Microsoft Visual C++ runtime. The package contains `AnimRef.exe`, the four media tools, and third-party license notices.

## Use the canvas

Right-click the canvas for Open, Save, Import, Paste, Copy, Export, layer ordering, Delete, grid/snap, view controls, playback loop boundaries, recent scenes, and window commands. Hover near the top edge to reveal the scene name and pin, minimize, maximize/restore, and close buttons. Right-drag moves the frameless window; left-drag moves a selected item, and its blue corner resizes it proportionally. Wheel zoom stays anchored at the pointer. Middle-drag or Space+left-drag pans the canvas.

Keyboard shortcuts: **Ctrl+L** opens a scene, **Ctrl+S** saves, **Ctrl+Shift+S** saves as, **Ctrl+N** creates a scene, **Ctrl+C** copies the selected content to the system clipboard, **Ctrl+V** pastes clipboard text, images, or files, **Ctrl+W** closes, **Ctrl+F** maximizes/restores, **Ctrl+M** minimizes, **Ctrl+G** toggles grid and snapping, and **Delete** removes the selection. For the selected animated item, **K** toggles playback and the arrow keys step frames; the floating playback bar provides the same controls and seeking.

Use **Copy** to put text, image pixels, or a media file on the system clipboard. **Export** writes the original image, GIF, video, or downloaded YouTube clip in its real format. Uncached YouTube items must be prepared before copying or exporting.

For YouTube, paste a URL and inspect metadata first. Choose the source start/end time as seconds or `HH:MM:SS`, then explicitly download that section. The app does not download the full video before trimming. yt-dlp and Deno handle YouTube extraction, FFmpeg cuts and decodes previews, and ffprobe verifies downloaded clips. Downloads can be cancelled, and the clip’s original URL and source-time mapping are stored in the scene.

## Validate

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

See [Windows environment setup](docs/ENVIRONMENT.md) and [revision acceptance notes](docs/REVISION_ACCEPTANCE.md). Media files and downloaded clips remain external to `.purgif` scenes.
