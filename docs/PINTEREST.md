# Pinterest import feasibility

AnimRef can import public Pinterest image and video Pins through the bundled yt-dlp extractor, without browser cookies or account access. Pinterest's documented v5 API requires an authenticated, authorized user; AnimRef does not request credentials or call that API. The importer uses yt-dlp's public Pinterest extractor and may show a clear unavailable/restricted error if Pinterest changes the public response.

The extractor emits video formats and duration for video Pins. For image Pins it can return image thumbnail URLs while producing no video formats; use `--ignore-no-formats-error --dump-single-json` to retain that metadata. An image is imported only when there are no video formats or positive duration, and the selected source is an HTTPS `i.pinimg.com` image. The downloaded bytes must decode as a supported image. If video evidence exists, AnimRef downloads the video and validates it with ffprobe rather than importing its poster frame.

Verified on 2026-10-17 with the bundled yt-dlp 2026.08.19:

| Kind | Public sample | Result |
| --- | --- | --- |
| Image | `https://www.pinterest.com/pin/2392606048574191/` | Metadata returned 9 image candidates. The original image URL returned HTTP 200 and 108,680 bytes; image decoding reported 750 × 1,166. |
| Video | `https://www.pinterest.com/pin/664281013778109217/` | Metadata returned 6 formats and 57.7 seconds. yt-dlp downloaded 18,403,864 bytes; ffprobe confirmed 1,080 × 1,920 and 57.7 seconds. |

The image and video files and a machine-readable probe record are in `target/acceptance/`. Public video availability can vary by Pin and Pinterest's current access rules.

References: [yt-dlp Pinterest extractor](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/pinterest.py), [yt-dlp image-pin no-formats issue](https://github.com/yt-dlp/yt-dlp/issues/15178), [Pinterest API authentication](https://developers.pinterest.com/docs/getting-started/set-up-authentication-and-authorization/).
