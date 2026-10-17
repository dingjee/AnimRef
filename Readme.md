# AnimRef

AnimRef is a Windows reference canvas for images, animated GIFs, text, video, YouTube sections, and public Pinterest Pins. The native Rust/egui app reads existing `.purgif` scenes, resolves relative media references, and saves playback position, loop ranges, source links, and grid settings.

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

Right-click a blank part of the canvas for Open, Save, Import, Paste, grid/snap, view controls, recent scenes, and window commands. Right-click an item for Copy, Export, layer ordering, Delete, video loop controls, and its source link when available. Hover near the top edge to reveal the centered scene name and icon buttons for pin, minimize, maximize/restore, and close. The canvas is black with an optional dotted grid. Right-drag moves the frameless window; left-drag moves an item, and its blue corner resizes it proportionally with a diagonal resize cursor. The selected animated item's playback bar follows it below the item. Wheel zoom stays anchored at the pointer. Middle-drag or Space+left-drag pans the canvas.

Keyboard shortcuts: **Ctrl+L** opens a scene, **Ctrl+S** saves, **Ctrl+Shift+S** saves as, **Ctrl+N** opens an empty new window, **Ctrl+C** copies the selected content to the system clipboard, **Ctrl+V** pastes clipboard text, images, or files, **Ctrl+W** closes, **Ctrl+F** maximizes/restores, **Ctrl+M** minimizes, **Ctrl+G** toggles grid and snapping, and **Delete** removes the selection. For the selected animated item, **K** toggles playback and the arrow keys step frames; the floating playback bar provides the same controls and seeking.

**Add text** starts editing directly on the canvas; double-click existing text to edit it. Its floating toolbar controls left/center/right alignment, Fibonacci font sizes, bold, and italic. Click outside or press Escape to finish editing; **Ctrl+S** also works while typing. Text styling is saved in `.purgif`, and older scenes retain their original text rendering until edited. **Center view** centers the combined item bounds without changing zoom. **Recent → Clear history** clears only the recent-path list. The filled pin indicates always-on-top; window edges and corners show the matching resize cursor.

Use **Copy** to put text, image pixels, or a media file on the system clipboard. **Export** writes the original image, GIF, video, or downloaded YouTube clip in its real format. Uncached YouTube items must be prepared before copying or exporting.

For YouTube, paste a URL and inspect metadata first. Choose the source start/end time as seconds or `HH:MM:SS`, then explicitly download that section. The app does not download the full video before trimming. yt-dlp and Deno handle YouTube extraction, FFmpeg cuts and decodes previews, and ffprobe verifies downloaded clips. Downloads can be cancelled, and the clip’s original URL and source-time mapping are stored in the scene.

Paste a public Pinterest Pin or `pin.it` link to import its image or video in the background. The app validates the actual media, caches it locally, and preserves the canonical Pin URL for **Open source link** in the item menu. This uses the bundled yt-dlp public extractor without browser cookies; restricted or unavailable Pins report an error. See [Pinterest feasibility and verified samples](docs/PINTEREST.md).

Window and playback controls use [Phosphor icons](https://github.com/amPerl/egui-phosphor). The wrapper and icon artwork licenses are included under `tools/licenses` and in the portable package.

## Validate

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

See [Windows environment setup](docs/ENVIRONMENT.md), [2.2.0 canvas and window acceptance](docs/CANVAS_EDIT_ACCEPTANCE.md), [2.1.1 UI and Pinterest acceptance](docs/UI_POLISH_ACCEPTANCE.md), and [earlier revision acceptance](docs/REVISION_ACCEPTANCE.md). Media files and downloaded clips remain external to `.purgif` scenes.
