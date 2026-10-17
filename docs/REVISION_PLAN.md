# Canvas and playback revision

This revision follows the user's 13 requests of 2026-10-17. Windows remains the only target. Existing `.purgif` files must continue to load, including legacy null geometry, source loop ranges and old video-edit camera recovery.

## Product decisions

- The canvas occupies the full client area. There is no permanent toolbar, inspector, status bar or welcome panel.
- A title overlay appears at the top edge on hover, with the scene name and pin/minimize/maximize/close controls. It does not change the canvas coordinates or reserve layout space.
- Important actions use one command path shared by context menus, keyboard shortcuts and the title overlay.
- Copy means the **system clipboard only**. It does not create another board element. Static images use image pixels; animated images and videos use a real file clipboard payload; text uses Unicode text.
- Export preserves the selected original image/animation/video, or the downloaded YouTube section. It does not silently export a screenshot or a loop-only transcode.
- A selected GIF or video has a small previous-frame/play-pause/next-frame overlay. Playback state and the last presented position are saved in the scene. Frame stepping pauses playback.
- Grid and snapping are a single per-scene option, off by default. Snapping uses world coordinates.
- YouTube preparation still fetches metadata first. The user chooses a range before explicitly downloading that section. Progress distinguishes section processing, finalization and verification.

## Implementation boundaries

The lead owns planning, review and acceptance. Luna implements the changes in the existing checkout; no additional worktree is used.

| Area | Owner | System change |
| --- | --- | --- |
| Scene model, GIF/video sessions, YouTube jobs | Luna environment | Versioned defaults, shared playback commands and snapshots, actual frame timestamps, bounded queues/history, structured progress from both process streams |
| Canvas, menus, platform adapter, clipboard/export, icon, build | Luna implementation | Deferred application commands, parented dialog coordinator, hover overlay, screen-coordinate window dragging, real media clipboard/export, embedded icon |
| Plan, independent review, acceptance record | Lead | Verify requirements and compatibility, inspect GPU screenshots, assess limitations, deliver tested package |

Only the UI owner runs Cargo while implementation is active, to avoid conflicting builds and toolchain environments. Backend and UI owners coordinate interface changes directly.

## Request-to-acceptance map

| Request | Required result / evidence |
| --- | --- |
| 1, 11: clean UI and hover title | Full canvas screenshot; title-overlay screenshot; title never changes camera/layout |
| 2: remove Edit video | No active edit view, toolbar action or keyboard entry; legacy scene decoding remains |
| 3: copy and export | Clipboard payload matches media kind; exported bytes and file format match the selected source, including embedded images and GIFs |
| 4: Open label and close menu | All scene-open actions say Open; popup closes before the native picker is invoked |
| 5: truthful YouTube progress | Structured section-time progress; unknown work stays indeterminate; completion follows verified cache publication |
| 6: dialog stacking | Native picker has the application as owner; pin is suspended before showing it and restored after cancellation or completion |
| 7: playback controls and persistence | Real source timestamps, pause/resume/previous/next, independent sessions, save/reload of the presented position and paused state |
| 8: grid and snap | Zoom-aware grid; positions and sizes snap in scene coordinates; option survives scene save/reload |
| 9: responsive layout | Narrow-window and normal-window screenshots of menus, title and YouTube dialog; no broken button wrapping |
| 10: YouTube close affordance | Upper-right X; closing cancels the associated job; no redundant bottom exit button |
| 12: icon | Same new geometric mark in the native viewport and Windows executable resources |
| 13: right-drag jitter | Drag threshold separates context click; movement is anchored to screen cursor and original window rectangle, without local-coordinate feedback |

## Verification strategy

Use focused model/media/progress/export tests, formatting, strict Clippy and release compilation. Render real application GPU snapshots for representative scenes and responsive layouts. Reopen legacy fixtures and the user's original scene read-only, recording its unchanged hash. Build and launch the portable distribution after the final source freeze.

Limit computer-use interaction as requested. The provided PureRef screenshot supplies the title-overlay reference. Use app-generated GPU captures for layout and animation checks, and a small native Open/cancel check for dialog stacking when the computer-use service is available. GPU captures alone do not establish native dialog stacking or physical window-drag behavior. State any remaining verification limits in the acceptance record.

The earlier migration acceptance in `ACCEPTANCE.md` is historical. Its screenshots, test counts and executable hashes must not be presented as evidence for this revision until the new release has been verified.
