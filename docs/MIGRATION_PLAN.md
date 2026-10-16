# AnimRef Rust migration

## Responsibilities

- Lead agent: source audit, architecture decision with the user, implementation guidance, independent acceptance.
- Luna implementation agent: Rust application, packaging, feature migration, implementation tests and repairs.
- Luna environment agent: Rust/linker/SDK installation and repair, media tools, reproducible environment setup. Environment mutations have a single owner to avoid installer conflicts.
- Work directly in the existing checkout. The user requested a final Git commit after cleanup and acceptance. No additional worktree or remote publication is required.

## Source baseline

The initial checkout is clean at `59a963c`. The application is Electron 30 with approximately 835 lines of preload logic, a small renderer, and native menu/clipboard/file handlers in `main.js`.

| Area | Existing behavior | Acceptance requirement |
| --- | --- | --- |
| Window | Frameless, initially 400×300, always on top; right-button drag moves window | Working desktop window, topmost toggle, move/resize, maximize/minimize/close |
| Import | File drop; clipboard file, bitmap, URL, text | Images, animated GIFs, local video, remote media, YouTube, and text can enter the board |
| Board | Select brings item to front; drag; proportional resizing; delete | Transform and stacking changes remain correct at non-unit zoom |
| Navigation | Wheel zoom anchored at pointer; middle drag/Space drag pan; Ctrl+Space drag zoom | Preserve usable navigation and avoid losing the scene offscreen |
| Playback | Muted autoplay and loop; fullscreen item editor; percentage trim range | Local video and YouTube play on the board and loop inside the selected interval |
| Editor | Enter/exit editing restores board transform | Editing does not corrupt layout; trim survives save/reload |
| Persistence | JSON `.purgif`, Save/Load/New, recent files, reopen latest | Read legacy files; round-trip items, view, z-order, and loop metadata; recover from missing/corrupt files |
| Shortcuts | Ctrl/Cmd+L/S/N/W/F/M; Ctrl+V; Delete | Desktop actions and board actions work without interfering with controls |
| Distribution | Windows/macOS/Linux Electron Forge jobs | User selected Windows only; provide documented Rust build/packaging and Windows CI |

### Legacy scene shape

The top-level object has `mode`, `currentScale`, `translate: { translateX, translateY }`, `workspaceRect: { x1, y1, x2, y2 }`, `elements`, and optional transient `editVideo` data. Elements store `path`, `type`, `x`, `y`, `width`, `height`; videos/YouTube also store `loopPairs` (percentage pairs, typically `[[0, 100]]`) and `activeLoopPair`. Array order is paint order. Known types include `img`, `dataURL`, `filePath`, `video`, `youtube`, and `text`. Width/height may be null, and extra/transient properties must not break reading.

### Source defects to avoid carrying forward

- YouTube accesses a cross-origin iframe's DOM and disables site isolation; use a supported playback bridge.
- Clipboard file paths are Windows-specific, bitmap PNG data is mislabeled as GIF, and pasted video URLs/files are treated as images.
- Only lowercase `.mp4` drops are recognized as videos.
- File loading can throw and terminate the application; recent paths may be stale.
- Touch pinch implementation is commented out; do not claim existing functional touch support.
- An edit-video scene can contain transient DOM-shaped data; persist stable domain data only.
- `Ctrl+H` is advertised by the empty screen but has no implementation; document or implement help honestly.

## Decisions

The user selected **Windows only**, with mandatory legacy `.purgif` compatibility. They then requested an elegant YouTube download approach and specifically asked to choose the trim range **before** manually downloading the corresponding content. Implement an eframe/egui Rust UI with `yt-dlp.exe`, FFmpeg/ffprobe and Deno as sidecars. No application JavaScript or WebView is needed.

### YouTube import contract

1. Pasting/importing a YouTube URL may fetch metadata, but must not download the full media.
2. Show source title/duration and editable source start/end times.
3. An explicit download action fetches the chosen section via `--download-sections "*START-END"` and FFmpeg. Do not implement this as downloading the full video and trimming it afterward.
4. Preserve source URL, duration, section boundaries and cached path. Cache identity distinguishes the source and millisecond-rounded ranges. This release uses a fixed best-compatible MP4 format selection; it does not expose quality variants. Playback loop percentages in a cached clip and timestamps in the original video must not be confused.
5. Legacy `youtube` elements retain their source URL and loop percentage metadata. Translate the active range into source time when preparing a download, preserving scene transforms and inactive loop pairs.
6. Provide progress, recoverable errors, cancellation, cache reuse, and a clear action to request another source range.
7. Exact transferred bytes depend on stream segmentation/keyframes; do not promise that the network transfers exactly the selected number of seconds. Offer accurate cuts through re-encoding when needed and verify the output duration with ffprobe.

References: [yt-dlp README](https://github.com/yt-dlp/yt-dlp), [official EJS/runtime guide](https://github.com/yt-dlp/yt-dlp/wiki/EJS). The official Windows standalone package includes EJS; Deno is a separate runtime. Download failure must remain visible and recoverable. Live YouTube success must be tested separately from the local download/cache mechanism.

## Work sequence

1. Audit source and agree technical boundaries.
2. Install/verify Rust stable, linker/SDK, and necessary media/browser dependencies.
3. Implement stable scene model and legacy compatibility; test malformed and round-trip scenes.
4. Implement the window, board interactions, imports, media playback, trim editor, persistence and recent history.
5. Replace entry points, document build/run/setup, and migrate CI/package generation.
6. Lead agent independently runs formatting, checks/tests, release build and available runtime acceptance; sends defects to Luna until resolved.

## Evidence required for completion

- Exact environment/tool versions and reproducible commands.
- Passing substantive scene/geometry/loop tests and compile/lint checks.
- A built Windows application and a runtime smoke check with logs or observed UI.
- Feature-by-feature evidence, with externally blocked checks explicitly recorded.
- No claims of tested macOS/Linux or live YouTube playback unless those tests actually run.
