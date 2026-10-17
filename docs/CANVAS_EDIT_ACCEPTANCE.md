# AnimRef 2.2.0 — canvas text and window revision

2026-10-17, Windows x64. Luna implemented the changes; the parent agent reviewed the code and performed independent release acceptance.

## Behavior

- The pin has one tooltip, a filled enabled icon, and an outline disabled icon. Header controls use a measured group with symmetric padding, including the close button.
- Canvas drawing and item hit testing share the same transformed rectangle. An opening right-click is no longer mistaken for an outside click when a menu is clamped away from the window edge. Right-drag uses Windows' physical drag thresholds.
- All window edges and corners select their corresponding resize cursor; interactive overlays retain their own pointer behavior.
- Center view centers the combined item bounds and preserves zoom. New / Ctrl+N launches a separate empty window with `--new-window`; it does not reload the last scene or clear the original window.
- Recent has a separator and Clear history. This writes an empty recent-path list, leaving scene files intact; other windows refresh the list when showing the submenu.
- Add text starts a canvas editor immediately; double-click edits existing text. The toolbar offers left/center/right alignment, Fibonacci font steps, bold, and italic. Escape or an outside click keeps the draft, and Ctrl+S remains available while editing. Text height grows for multiline content. Editing suppresses the static text painter to avoid duplicate glyphs.
- Optional `textStyle` metadata persists styling. Old scenes without it retain their legacy auto-fit painting until edited. Dedicated text font families preserve the application's icon fonts; italic uses the same synthetic skew in editing and display.
- The independently drawn blue/violet SVG icon has a large circular bowl and balanced short legs. The straight left handle has parallel 45-degree edges and a foot aligned with the right stem. PNG and five-size ICO are generated from that SVG with supersampling.

## Verification

The local suite exercises scene compatibility, style round trips, Fibonacci boundaries, real FFmpeg/GIF/VFR playback, editor layout and height, editing completion, transformed item targeting, popup opening/dismissal, centering, new-window arguments, history persistence, and edge/corner cursors. The two network-download tests were excluded in this revision; their backends were unchanged and their earlier results are recorded in the 2.1.1 acceptance document.

Final local results: **60 tests passed** (37 library and 23 application), formatting passed, and strict Clippy passed. Additional editor regressions send real consecutive-frame `Event::Text` input after Add Text and compare actual editable/static glyph bounds for all three alignments. The editable galley origin is normalized before TextEdit alignment; painting uses the corresponding anchor, keeping both paths visually consistent.

Actual release GPU frames are captured using the application's diagnostic CLI at normal and minimum window sizes. Reports include the process ID, actual viewport, DPI scale, overlay rectangles, editor state, media errors, and frame counts. These captures inspect rendering; unit tests drive egui pointer/key input for interaction behavior. This revision does not claim a full manual Windows interaction sweep.

Selected captures at the machine's **150% Windows scale**:

- [Filled pin and header](acceptance/canvas-edit-title.png), [outline pin](acceptance/canvas-edit-title-unpinned.png), [minimum header](acceptance/canvas-edit-title-minimum.png).
- [Canvas text editor](acceptance/canvas-edit-text-edit.png), [minimum editor and toolbar](acceptance/canvas-edit-text-edit-minimum.png), [bold italic text](acceptance/canvas-edit-text-styled.png), [three text alignments](acceptance/canvas-edit-alignment.png).
- [Left-side item menu at minimum size](acceptance/canvas-edit-menu-left-minimum.png), [Recent with Clear history](acceptance/canvas-edit-recent-menu.png).

The original user scene is opened read-only during acceptance. Its SHA-256 before and after is checked against `43645D32574CB91DE2AF9724C599648C9AE6611AEBECA91052487BD94D22F22C`; all test writes stay under ignored `target/acceptance`.

## Reproduce

```powershell
cargo fmt --all -- --check
cargo test --locked -- --include-ignored --skip downloads_a_fresh_youtube_section_and_reports_real_ffmpeg_progress --skip downloads_public_image_and_video_using_job_api
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
./scripts/package-windows.ps1 -SkipBuild
```

The portable folder and ZIP must contain the accepted executable, FFmpeg, ffprobe, yt-dlp, Deno, documentation, and license notices. The executable's embedded icon payloads are compared to `assets/animref.ico`; packaged executable and tools are compared with their accepted sources by SHA-256.
