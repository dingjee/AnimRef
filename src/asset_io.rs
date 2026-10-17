use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use image::ImageFormat;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::Element;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Gif,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMedia {
    pub path: PathBuf,
    pub kind: AssetKind,
    pub suggested_name: String,
}

pub fn prepare(element: &Element) -> Result<PreparedMedia> {
    let cache_root = directories::ProjectDirs::from("", "AnimRef", "AnimRef")
        .map(|dirs| dirs.cache_dir().join("assets"))
        .unwrap_or_else(|| std::env::temp_dir().join("AnimRef-cache").join("assets"));
    prepare_with_cache_dir(element, &cache_root)
}

pub fn image_rgba(prepared: &PreparedMedia) -> Result<image::RgbaImage> {
    match prepared.kind {
        AssetKind::Video => bail!("Video files cannot be decoded as still images."),
        AssetKind::Image | AssetKind::Gif => {}
    }
    Ok(image::open(&prepared.path)
        .with_context(|| format!("Could not decode image: {}", prepared.path.display()))?
        .to_rgba8())
}

fn prepare_with_cache_dir(element: &Element, cache_root: &Path) -> Result<PreparedMedia> {
    if let Some(cached_path) = element.cached_path.as_deref().filter(|path| path.is_file()) {
        return prepare_local_file(element, cached_path, cache_root);
    }
    if is_data_url(&element.path) {
        let bytes = decode_data_url(element.path.trim())?;
        return prepare_image_bytes(&bytes, cache_root, Some("image"));
    }

    if is_remote_url(&element.path) || element.media_type == "youtube" {
        if element.media_type == "youtube" {
            bail!("This YouTube clip is not cached yet. Choose a section to download it first.");
        }
        bail!("This remote media is not cached yet. Add a local file before using it here.");
    }
    let source = PathBuf::from(&element.path);
    if !source.is_file() {
        bail!("The media file is unavailable: {}", element.path);
    }

    prepare_local_file(element, &source, cache_root)
}

fn prepare_local_file(
    element: &Element,
    source: &Path,
    cache_root: &Path,
) -> Result<PreparedMedia> {
    let original_name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("media");
    let extension = extension(source);
    let prefix = read_prefix(source, 64 * 1024)?;

    if let Some((kind, actual_extension)) = sniff_video_container(&prefix) {
        let path = if extension_matches(&extension, actual_extension) {
            source.to_path_buf()
        } else {
            materialize_file(source, cache_root, actual_extension)?
        };
        return Ok(PreparedMedia {
            path,
            kind: AssetKind::Video,
            suggested_name: format!(
                "{}.{}",
                suggested_stem(element, original_name, kind),
                actual_extension
            ),
        });
    }

    if image::guess_format(&prefix).is_ok() {
        let bytes = fs::read(source)
            .with_context(|| format!("Could not read image: {}", source.display()))?;
        return prepare_local_image_bytes(&bytes, source, &extension, original_name, cache_root);
    }

    if is_known_video_extension(&extension) || element.is_video() {
        let actual_extension = if is_known_video_extension(&extension) {
            extension.as_str()
        } else {
            bail!("The video container could not be identified from its file header.");
        };
        return Ok(PreparedMedia {
            path: source.to_path_buf(),
            kind: AssetKind::Video,
            suggested_name: format!(
                "{}.{}",
                suggested_stem(element, original_name, AssetKind::Video),
                actual_extension
            ),
        });
    }

    if is_image_extension(&extension) || matches!(element.media_type.as_str(), "img" | "gif") {
        let bytes = fs::read(source)
            .with_context(|| format!("Could not read image: {}", source.display()))?;
        return prepare_local_image_bytes(&bytes, source, &extension, original_name, cache_root);
    }

    bail!(
        "The file format is not a supported image or video: {}",
        source.display()
    )
}

fn prepare_image_bytes(
    bytes: &[u8],
    cache_root: &Path,
    stem: Option<&str>,
) -> Result<PreparedMedia> {
    let (kind, extension) = validate_image_bytes(bytes)?;
    let path = materialize_bytes(bytes, cache_root, extension)?;
    Ok(PreparedMedia {
        path,
        kind,
        suggested_name: format!("{}.{}", stem.unwrap_or("image"), extension),
    })
}

fn prepare_local_image_bytes(
    bytes: &[u8],
    source: &Path,
    source_extension: &str,
    stem: &str,
    cache_root: &Path,
) -> Result<PreparedMedia> {
    let (kind, actual_extension) = validate_image_bytes(bytes)?;
    let path = if extension_matches(source_extension, actual_extension) {
        source.to_path_buf()
    } else {
        materialize_bytes(bytes, cache_root, actual_extension)?
    };
    Ok(PreparedMedia {
        path,
        kind,
        suggested_name: format!("{stem}.{actual_extension}"),
    })
}

fn validate_image_bytes(bytes: &[u8]) -> Result<(AssetKind, &'static str)> {
    let format =
        image::guess_format(bytes).context("Embedded or local media is not a supported image")?;
    let extension = image_extension(&format)
        .ok_or_else(|| anyhow!("This image format is not supported for export."))?;
    image::load_from_memory_with_format(bytes, format)
        .context("Image data could not be decoded")?;
    let kind = if format == ImageFormat::Gif {
        AssetKind::Gif
    } else {
        AssetKind::Image
    };
    Ok((kind, extension))
}

fn decode_data_url(value: &str) -> Result<Vec<u8>> {
    let (scheme, body) = value.split_once(':').context("Invalid data URL")?;
    if !scheme.eq_ignore_ascii_case("data") {
        bail!("Invalid data URL");
    }
    let (metadata, payload) = body
        .split_once(',')
        .context("Invalid data URL: missing payload")?;
    let is_base64 = metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"));
    let bytes = if is_base64 {
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload))
            .context("Invalid base64 data URL payload")?
    } else {
        percent_decode(payload)?
    };
    Ok(bytes)
}

fn percent_decode(value: &str) -> Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1).context("Invalid percent escape")?;
            let low = *bytes.get(index + 2).context("Invalid percent escape")?;
            decoded.push((hex_nibble(high)? << 4) | hex_nibble(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("Invalid percent escape"),
    }
}

fn is_remote_url(value: &str) -> bool {
    let value = value.trim();
    if value.starts_with("//") {
        return true;
    }
    let Some((scheme, _)) = value.split_once("://") else {
        return false;
    };
    let is_windows_drive = scheme.len() == 1 && scheme.as_bytes()[0].is_ascii_alphabetic();
    !is_windows_drive
        && !scheme.is_empty()
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
}

fn is_data_url(value: &str) -> bool {
    value
        .trim()
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

fn read_prefix(path: &Path, max_len: usize) -> Result<Vec<u8>> {
    let mut file =
        File::open(path).with_context(|| format!("Could not open media: {}", path.display()))?;
    let mut prefix = vec![0; max_len];
    let count = file.read(&mut prefix)?;
    prefix.truncate(count);
    Ok(prefix)
}

fn sniff_video_container(bytes: &[u8]) -> Option<(AssetKind, &'static str)> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        let extension = if brand == b"qt  " {
            "mov"
        } else if brand == b"M4V " {
            "m4v"
        } else {
            "mp4"
        };
        return Some((AssetKind::Video, extension));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"AVI " {
        return Some((AssetKind::Video, "avi"));
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        let header = &bytes[..bytes.len().min(64 * 1024)];
        if header.windows(4).any(|part| part == b"webm") {
            return Some((AssetKind::Video, "webm"));
        }
        if header.windows(8).any(|part| part == b"matroska") {
            return Some((AssetKind::Video, "mkv"));
        }
    }
    None
}

fn image_extension(format: &ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpg"),
        ImageFormat::Gif => Some("gif"),
        ImageFormat::WebP => Some("webp"),
        ImageFormat::Bmp => Some("bmp"),
        _ => None,
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn extension_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
        || (expected == "jpg" && actual.eq_ignore_ascii_case("jpeg"))
}

fn is_image_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
}

fn is_known_video_extension(extension: &str) -> bool {
    matches!(extension, "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi")
}

fn suggested_stem(element: &Element, local_stem: &str, kind: AssetKind) -> String {
    if is_data_url(&element.path) {
        "image".into()
    } else if element.media_type == "youtube" {
        if kind == AssetKind::Video {
            "youtube-clip".into()
        } else {
            "image".into()
        }
    } else if is_remote_url(&element.path) {
        if kind == AssetKind::Video {
            "media".into()
        } else {
            "image".into()
        }
    } else {
        local_stem.to_owned()
    }
}

fn materialize_bytes(bytes: &[u8], cache_root: &Path, extension: &str) -> Result<PathBuf> {
    let digest = Sha256::digest(bytes);
    let target = cache_root.join(format!("{}.{}", hex(&digest), extension));
    atomic_write(&target, |file| file.write_all(bytes))
}

fn materialize_file(source: &Path, cache_root: &Path, extension: &str) -> Result<PathBuf> {
    let mut source_file = File::open(source)
        .with_context(|| format!("Could not open media: {}", source.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let target = cache_root.join(format!("{}.{}", hex(&hasher.finalize()), extension));
    atomic_write(&target, |destination| {
        source_file.rewind()?;
        std::io::copy(&mut source_file, destination)?;
        Ok(())
    })
}

fn atomic_write(
    target: &Path,
    write_content: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<PathBuf> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
    let parent = target
        .parent()
        .context("Asset cache has no parent directory")?;
    fs::create_dir_all(parent).context("Could not create the asset cache")?;
    if target.is_file() {
        return Ok(target.to_path_buf());
    }
    let mut attempts = 0;
    let (temp, mut file) = loop {
        if attempts >= 32 {
            return Err(anyhow!("Could not allocate a unique temporary asset file."));
        }
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".asset-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempts += 1;
            }
            Err(error) => return Err(error).context("Could not create a temporary asset file"),
        }
    };
    let write_result = write_content(&mut file).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error).context("Could not write the asset cache file");
    }
    match fs::rename(&temp, target) {
        Ok(()) => Ok(target.to_path_buf()),
        Err(_error) if target.is_file() => {
            let _ = fs::remove_file(&temp);
            Ok(target.to_path_buf())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
                .with_context(|| format!("Could not publish cached asset {}", target.display()))
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(CHARS[(byte >> 4) as usize] as char);
        result.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use image::{Delay, Frame, Rgba, RgbaImage};
    use std::io::Cursor;
    use tempfile::TempDir;

    fn png_bytes() -> Vec<u8> {
        let image = RgbaImage::from_pixel(3, 2, Rgba([12, 34, 56, 255]));
        let mut cursor = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .expect("encode PNG fixture");
        cursor.into_inner()
    }

    fn gif_bytes() -> Vec<u8> {
        let first = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let second = RgbaImage::from_pixel(2, 2, Rgba([0, 255, 0, 255]));
        let frames = vec![
            Frame::from_parts(first, 0, 0, Delay::from_numer_denom_ms(80, 1)),
            Frame::from_parts(second, 0, 0, Delay::from_numer_denom_ms(140, 1)),
        ];
        let mut cursor = Cursor::new(Vec::new());
        image::codecs::gif::GifEncoder::new(&mut cursor)
            .encode_frames(frames)
            .expect("encode animated GIF fixture");
        cursor.into_inner()
    }

    fn data_url(mime: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn legacy_gif_data_url_with_png_bytes_exports_as_png() {
        let directory = TempDir::new().expect("create isolated cache");
        let bytes = png_bytes();
        let element = Element::new(data_url("image/gif", &bytes), "gif", 0.0, 0.0, 3.0, 2.0);

        let prepared = prepare_with_cache_dir(&element, directory.path()).unwrap();

        assert_eq!(prepared.kind, AssetKind::Image);
        assert_eq!(
            prepared.path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert_eq!(prepared.suggested_name, "image.png");
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
    }

    #[test]
    fn animated_gif_data_url_keeps_gif_kind_and_original_bytes() {
        let directory = TempDir::new().expect("create isolated cache");
        let bytes = gif_bytes();
        let element = Element::new(data_url("image/png", &bytes), "img", 0.0, 0.0, 2.0, 2.0);

        let prepared = prepare_with_cache_dir(&element, directory.path()).unwrap();

        assert_eq!(prepared.kind, AssetKind::Gif);
        assert_eq!(
            prepared.path.extension().and_then(|ext| ext.to_str()),
            Some("gif")
        );
        assert_eq!(prepared.suggested_name, "image.gif");
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
    }

    #[test]
    fn local_mislabeled_image_gets_a_real_extension_and_exact_bytes() {
        let directory = TempDir::new().expect("create isolated directory");
        let cache = directory.path().join("cache");
        let source = directory.path().join("still.gif");
        let bytes = png_bytes();
        fs::write(&source, &bytes).unwrap();
        let element = Element::new(
            source.to_string_lossy().into_owned(),
            "img",
            0.0,
            0.0,
            3.0,
            2.0,
        );

        let prepared = prepare_with_cache_dir(&element, &cache).unwrap();

        assert_eq!(prepared.kind, AssetKind::Image);
        assert_eq!(
            prepared.path.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert_ne!(prepared.path, source);
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
    }

    #[test]
    fn valid_cached_path_takes_precedence_over_original_data_url() {
        let directory = TempDir::new().expect("create isolated directory");
        let bytes = gif_bytes();
        let cached = directory.path().join("cached.gif");
        fs::write(&cached, &bytes).unwrap();
        let mut element = Element::new(
            data_url("image/png", &png_bytes()),
            "img",
            0.0,
            0.0,
            2.0,
            2.0,
        );
        element.cached_path = Some(cached.clone());

        let prepared = prepare_with_cache_dir(&element, &directory.path().join("cache")).unwrap();

        assert_eq!(prepared.kind, AssetKind::Gif);
        assert_eq!(prepared.path, cached);
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
    }

    #[test]
    fn local_video_is_preserved_as_a_file_and_rejected_as_a_bitmap() {
        let directory = TempDir::new().expect("create isolated directory");
        let bytes = b"\0\0\0\x18ftypqt  \0\0\0\0quicktime";
        let source = directory.path().join("reference.mov");
        fs::write(&source, bytes).unwrap();
        let element = Element::new(
            source.to_string_lossy().into_owned(),
            "video",
            0.0,
            0.0,
            5.0,
            3.0,
        );

        let prepared = prepare_with_cache_dir(&element, &directory.path().join("cache")).unwrap();

        assert_eq!(prepared.kind, AssetKind::Video);
        assert_eq!(prepared.path, source);
        assert_eq!(prepared.suggested_name, "reference.mov");
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
        assert!(image_rgba(&prepared).is_err());
    }

    #[test]
    fn local_video_with_wrong_extension_is_copied_with_detected_container_extension() {
        let directory = TempDir::new().expect("create isolated directory");
        let bytes = b"\0\0\0\x18ftypisom\0\0\0\0isomiso2";
        let source = directory.path().join("reference.gif");
        fs::write(&source, bytes).unwrap();
        let element = Element::new(
            source.to_string_lossy().into_owned(),
            "filePath",
            0.0,
            0.0,
            5.0,
            3.0,
        );

        let prepared = prepare_with_cache_dir(&element, &directory.path().join("cache")).unwrap();

        assert_eq!(prepared.kind, AssetKind::Video);
        assert_eq!(
            prepared.path.extension().and_then(|ext| ext.to_str()),
            Some("mp4")
        );
        assert_ne!(prepared.path, source);
        assert_eq!(fs::read(&prepared.path).unwrap(), bytes);
    }

    #[test]
    fn uncached_remote_and_youtube_media_fail_without_network_access() {
        let directory = TempDir::new().expect("create isolated cache");
        let remote = Element::new(
            "https://media.example.invalid/clip.mp4".into(),
            "filePath",
            0.0,
            0.0,
            10.0,
            10.0,
        );
        let youtube = Element::new(
            "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
            "youtube",
            0.0,
            0.0,
            10.0,
            10.0,
        );

        let remote_error = prepare_with_cache_dir(&remote, directory.path()).unwrap_err();
        let youtube_error = prepare_with_cache_dir(&youtube, directory.path()).unwrap_err();

        assert!(remote_error.to_string().contains("not cached"));
        assert!(youtube_error.to_string().contains("Choose a section"));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
