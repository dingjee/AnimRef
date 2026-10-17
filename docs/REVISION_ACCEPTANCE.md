# Canvas and playback revision acceptance

Revision of the native Windows Rust application, following the 13 requests mapped in [REVISION_PLAN.md](REVISION_PLAN.md). Acceptance was performed on Windows on 2026-10-17.

## Source review

The lead reviewed command routing, native dialog ownership/topmost restoration, screen-coordinate window dragging, playback queue/timestamp handling, YouTube progress and cancellation, and clipboard/export data handling. Issues found during review were returned to Luna for repair before final build acceptance.

## Build and regression results

The final local regression run passed **37 tests** (24 library, 13 binary), including the opt-in FFmpeg tests. Only the network test was filtered from that final run; it had already passed independently in the earlier full 32-test run, with no subsequent YouTube backend changes. The fresh two-second download reported 2.000 seconds and 85,002 output bytes, and verified cache reuse. Logs: `target/acceptance/revision-final-local-tests.log` and `revision-tests.log`.

Regressions cover VFR source timestamps, GIF alpha/delays, pause holding, independent playback sessions, worker teardown, legacy scene migration, playback/grid persistence, original media formats for export, clipboard command routing, pointer-anchored zoom, primary drag snapping, and isolation of secondary dragging. An actual egui multi-frame test verifies that displaying a fractional playhead without input does not queue seeks; another verifies that video-frame report updates cannot postpone diagnostic exit indefinitely. Formatting and strict Clippy passed.

The lead's PE resource parser confirmed all five ICO payloads are embedded in the release executable and match the source: 16, 32, 48, 64 and 256 pixels.

Release executable SHA-256: `402C915CF48AF88F0D4B115379DD9B7057D329B4B7CBA90DCC830A15DA390D96`.

## GPU snapshots

The local harness is `target/acceptance/run-revision-snapshots.ps1`. It renders clean canvas, hover title, selected playback controls, grid, narrow context menu and YouTube form, cached YouTube playback, legacy media, and the user's original scene. These are actual egui GPU frames emitted by the application, not composited mockups.

The final release passed **all 14 cases**, including 360x480 and the actual 320x220 minimum viewport. Every process exited successfully after producing a fresh screenshot and matching process-ID report. Cached YouTube played 64 frames over the capture interval; selected-video playback is checked per item, so another session cannot mask a stall. Paused VFR playback stayed at 0.400 seconds. Embedded GIF and mislabeled PNG fixtures rendered correctly. The legacy missing-file fixture produced its one intentional image error; all other cases had zero image/decoder errors.

The user's original two-item scene was opened read-only and its SHA-256 remained `D33E3E0FEAA1B17D520C4CC52DEFEFFBFFDE2CB050739F582AA2FAF00A159D67`. Logs and reports are under `target/acceptance/revision-*`. The narrow YouTube form uses deterministic long metadata to exercise both time fields without another network download; the minimum-height form scrolls.

Reviewed captures: [clean canvas](acceptance/native-board.png), [hover title](acceptance/revision-title.png), [playback](acceptance/revision-playback.png), [grid](acceptance/revision-grid.png), [narrow menu](acceptance/revision-menu-narrow.png), [YouTube range form](acceptance/revision-youtube-narrow.png), [minimum-height form](acceptance/revision-youtube-minimum.png).

## Distribution

The portable folder's executable completed a five-item playback startup check, emitted a rendered GPU screenshot, decoded 93 frames and exited successfully. The final package is `dist/AnimRef-Windows.zip`; the adjacent `.sha256` file records its hash. The package verifier checks the folder and archive against the accepted executable and all four media sidecars, and requires the README, project license, revision acceptance record and third-party notices. Local evidence: `target/acceptance/revision-packaged.json` and `revision-package-hashes.json`.

Build output, local fixtures, cached downloads and the portable package remain ignored by Git. The obsolete Electron application files were removed in the earlier migration; historical migration documents and screenshots are explicitly labeled and retained for provenance.

## Verification limits

The Windows computer-use service recovered sufficiently to list and launch the final application, but window capture failed with `FrameArrived timed out: timed out waiting on channel`; refreshing selection and activation once then failed with `window capture timed out: timed out waiting on channel`. No native input was sent. Consequently native dialog z-order, physical right-button window dragging, and clipboard interoperability with other applications remain unverified by live UI automation. The code paths were reviewed, and egui input regressions cover secondary-drag isolation and clipboard routing. The provided PureRef screenshot was used as the visual title-overlay reference. GPU snapshots establish application rendering and runtime playback, not those physical interactions.

Windows on this machine is the acceptance target. No clean Windows VM or remote CI run is claimed.
