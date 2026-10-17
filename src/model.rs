use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub const CURRENT_SCENE_VERSION: u32 = 3;
static NEXT_ELEMENT_ID: AtomicU64 = AtomicU64::new(1);

fn deserialize_null_f32_as_zero<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f32>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_null_f64_as_zero<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_null_scale_as_one<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f32>::deserialize(deserializer)?.unwrap_or_else(default_scale))
}

fn is_youtube_video_url(value: &str) -> bool {
    let Some((scheme, address)) = value.trim().split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority_end = address.find(['/', '?', '#']).unwrap_or(address.len());
    let authority = &address[..authority_end];
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let remainder = &address[authority_end..];
    let (path, query) = remainder.split_once('?').unwrap_or((remainder, ""));

    let video_id = if host == "youtu.be" {
        path.trim_start_matches('/')
            .split(['/', '#'])
            .next()
            .unwrap_or_default()
    } else if matches!(
        host.as_str(),
        "youtube.com"
            | "www.youtube.com"
            | "m.youtube.com"
            | "music.youtube.com"
            | "youtube-nocookie.com"
            | "www.youtube-nocookie.com"
    ) {
        if path == "/watch" {
            query
                .split('&')
                .find_map(|pair| {
                    let (key, value) = pair.split_once('=')?;
                    key.eq_ignore_ascii_case("v").then_some(value)
                })
                .unwrap_or_default()
        } else {
            let mut segments = path.trim_matches('/').split('/');
            match segments.next().unwrap_or_default() {
                "shorts" | "live" | "embed" | "v" => segments.next().unwrap_or_default(),
                _ => "",
            }
        }
    } else {
        return false;
    };

    video_id.len() == 11
        && video_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Accept only an absolute HTTP(S) URL with an unambiguous public authority.
/// This is intentionally small and conservative because the value is handed to
/// the operating system's browser opener.
fn is_safe_source_url(value: &str) -> bool {
    let value = value.trim();
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || value.contains('\\')
    {
        return false;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let mut authority_parts = authority.split(':');
    let host = authority_parts
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    if let Some(port) = authority_parts.next()
        && (authority_parts.next().is_some()
            || port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|port| port == 0))
    {
        return false;
    }
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && !host.contains("..")
        && host
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
}

fn fresh_element_id() -> u64 {
    NEXT_ELEMENT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Translate {
    #[serde(
        default,
        rename = "translateX",
        deserialize_with = "deserialize_null_f32_as_zero"
    )]
    pub x: f32,
    #[serde(
        default,
        rename = "translateY",
        deserialize_with = "deserialize_null_f32_as_zero"
    )]
    pub y: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WorkspaceRect {
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub x1: f32,
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub y1: f32,
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub x2: f32,
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub y2: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Element {
    #[serde(default = "fresh_element_id")]
    pub id: u64,
    #[serde(default)]
    pub path: String,
    #[serde(default, rename = "sourceUrl", skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, rename = "type")]
    pub media_type: String,
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub x: f32,
    #[serde(default, deserialize_with = "deserialize_null_f32_as_zero")]
    pub y: f32,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
    #[serde(default, rename = "loopPairs")]
    pub loop_pairs: Vec<[f64; 2]>,
    #[serde(default, rename = "activeLoopPair")]
    pub active_loop_pair: usize,
    #[serde(
        default,
        rename = "cachedPath",
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_path: Option<PathBuf>,
    #[serde(
        default,
        rename = "sourceDuration",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_duration: Option<f64>,
    #[serde(
        default,
        rename = "mediaDuration",
        skip_serializing_if = "Option::is_none"
    )]
    pub media_duration: Option<f64>,
    #[serde(
        default,
        rename = "sourceRange",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_range: Option<[f64; 2]>,
    #[serde(
        default,
        rename = "playbackLoopPairs",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub playback_loop_pairs: Vec<[f64; 2]>,
    #[serde(
        default,
        rename = "activePlaybackLoopPair",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_playback_loop_pair: Option<usize>,
    #[serde(default, rename = "playbackState")]
    pub playback_state: PlaybackState,
    #[serde(default, rename = "textStyle", skip_serializing_if = "Option::is_none")]
    pub text_style: Option<TextStyle>,
}

/// Persistent formatting for newly styled canvas text. `None` keeps legacy text
/// on its original auto-fit rendering path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default = "default_text_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub alignment: TextAlignment,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
}

const TEXT_FONT_STEPS: [f32; 14] = [
    1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 21.0, 34.0, 55.0, 89.0, 144.0, 233.0, 377.0, 610.0,
];

fn default_text_font_size() -> f32 {
    21.0
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_size: default_text_font_size(),
            alignment: TextAlignment::Left,
            bold: false,
            italic: false,
        }
    }
}

impl TextStyle {
    /// Repair invalid persisted sizes into the supported Fibonacci range.
    pub fn normalize(&mut self) {
        if !self.font_size.is_finite() || self.font_size <= 0.0 {
            self.font_size = default_text_font_size();
        }
        self.font_size = self
            .font_size
            .clamp(TEXT_FONT_STEPS[0], *TEXT_FONT_STEPS.last().unwrap());
    }

    /// Move to the next Fibonacci size. Arbitrary legacy sizes step to the
    /// nearest strictly larger/smaller supported value.
    pub fn step_font_size(&mut self, direction: i32) {
        self.normalize();
        if direction > 0 {
            if let Some(next) = TEXT_FONT_STEPS
                .iter()
                .copied()
                .find(|size| *size > self.font_size)
            {
                self.font_size = next;
            }
        } else if direction < 0
            && let Some(previous) = TEXT_FONT_STEPS
                .iter()
                .rev()
                .copied()
                .find(|size| *size < self.font_size)
        {
            self.font_size = previous;
        }
    }
}

impl Element {
    /// Original remote source for opening in the system browser, if safe.
    /// Legacy YouTube scenes stored their original URL in `path`.
    pub fn source_link(&self) -> Option<&str> {
        if let Some(url) = self.source_url.as_deref()
            && is_safe_source_url(url)
        {
            return Some(url);
        }
        (self.media_type == "youtube"
            && is_youtube_video_url(&self.path)
            && is_safe_source_url(&self.path))
        .then_some(self.path.as_str())
    }

    pub fn new(path: String, media_type: &str, x: f32, y: f32, width: f32, height: f32) -> Self {
        let is_video = matches!(media_type, "video" | "youtube");
        Self {
            id: fresh_element_id(),
            path,
            source_url: None,
            media_type: media_type.to_owned(),
            x,
            y,
            width: Some(width),
            height: Some(height),
            loop_pairs: if is_video {
                vec![[0.0, 100.0]]
            } else {
                Vec::new()
            },
            active_loop_pair: 0,
            cached_path: None,
            source_duration: None,
            media_duration: None,
            source_range: None,
            playback_loop_pairs: Vec::new(),
            active_playback_loop_pair: None,
            playback_state: PlaybackState::default(),
            text_style: None,
        }
    }

    pub fn is_video(&self) -> bool {
        matches!(self.media_type.as_str(), "video" | "youtube")
    }

    pub fn display_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.cached_path
            && path.exists()
        {
            return Some(path.clone());
        }
        if self.media_type == "youtube" {
            return None;
        }
        Some(PathBuf::from(&self.path))
    }

    pub fn active_playback_pair(&self) -> usize {
        self.active_playback_loop_pair
            .unwrap_or(self.active_loop_pair)
    }

    #[cfg(test)]
    fn source_download_range(&self) -> Option<[f64; 2]> {
        if let Some(range) = self.source_range {
            return Some(range);
        }
        let duration = self.source_duration?;
        let pair = self.loop_pairs.get(self.active_loop_pair)?;
        Some([duration * pair[0] / 100.0, duration * pair[1] / 100.0])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    #[serde(
        default,
        rename = "formatVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub format_version: Option<u32>,
    #[serde(
        default = "default_scale",
        rename = "currentScale",
        deserialize_with = "deserialize_null_scale_as_one"
    )]
    pub current_scale: f32,
    #[serde(default)]
    pub translate: Translate,
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default, rename = "workspaceRect")]
    pub workspace_rect: WorkspaceRect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, rename = "editVideo", skip_serializing_if = "Option::is_none")]
    pub edit_video: Option<Value>,
    #[serde(default)]
    pub grid: GridSettings,
}

fn default_scale() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaybackState {
    #[serde(default)]
    pub paused: bool,
    #[serde(default, deserialize_with = "deserialize_null_f64_as_zero")]
    pub position_seconds: f64,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            paused: false,
            position_seconds: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(
        default = "default_grid_spacing",
        deserialize_with = "deserialize_null_f32_as_zero"
    )]
    pub spacing: f32,
}

fn default_grid_spacing() -> f32 {
    32.0
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            spacing: default_grid_spacing(),
        }
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            format_version: Some(CURRENT_SCENE_VERSION),
            current_scale: 1.0,
            translate: Translate::default(),
            elements: Vec::new(),
            workspace_rect: WorkspaceRect::default(),
            mode: Some("standard".into()),
            edit_video: None,
            grid: GridSettings::default(),
        }
    }
}

impl Scene {
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let mut scene: Self = serde_json::from_str(text)?;
        if scene.mode.as_deref() == Some("edit-video")
            && let Some(backup) = scene
                .edit_video
                .as_ref()
                .and_then(|edit| edit.get("backupState"))
        {
            if let Some(scale) = backup.get("currentScale").and_then(Value::as_f64)
                && scale.is_finite()
                && scale > 0.0
            {
                scene.current_scale = scale as f32;
            }
            if let Some(translate) = backup.get("translate") {
                if let Some(x) = translate.get("translateX").and_then(Value::as_f64)
                    && x.is_finite()
                {
                    scene.translate.x = x as f32;
                }
                if let Some(y) = translate.get("translateY").and_then(Value::as_f64)
                    && y.is_finite()
                {
                    scene.translate.y = y as f32;
                }
            }
        }
        scene.format_version = Some(CURRENT_SCENE_VERSION);
        scene.mode = Some("standard".into());
        scene.edit_video = None;
        if !scene.grid.spacing.is_finite() || scene.grid.spacing < 4.0 || scene.grid.spacing > 512.0
        {
            scene.grid.spacing = default_grid_spacing();
        }
        let mut seen_ids = std::collections::HashSet::new();
        for element in &mut scene.elements {
            if let Some(style) = &mut element.text_style {
                style.normalize();
            }
            if !element.playback_state.position_seconds.is_finite()
                || element.playback_state.position_seconds < 0.0
            {
                element.playback_state.position_seconds = 0.0;
            }
            if matches!(element.media_type.as_str(), "img" | "filePath")
                && is_youtube_video_url(&element.path)
            {
                element.media_type = "youtube".into();
            }
            if element.source_url.is_none()
                && element.media_type == "youtube"
                && is_youtube_video_url(&element.path)
                && is_safe_source_url(&element.path)
            {
                element.source_url = Some(element.path.clone());
            }
            if element.id == 0 || !seen_ids.insert(element.id) {
                element.id = fresh_element_id();
                seen_ids.insert(element.id);
            } else {
                NEXT_ELEMENT_ID.fetch_max(element.id.saturating_add(1), Ordering::Relaxed);
            }
            if element.width.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                element.width = None;
            }
            if element.height.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                element.height = None;
            }
            element.loop_pairs.retain(|p| valid_pair(*p));
            element.playback_loop_pairs.retain(|p| valid_pair(*p));
            if element.loop_pairs.is_empty() && element.is_video() {
                element.loop_pairs.push([0.0, 100.0]);
            }
            element.active_loop_pair = element
                .active_loop_pair
                .min(element.loop_pairs.len().saturating_sub(1));
            if !element.playback_loop_pairs.is_empty() {
                element.active_playback_loop_pair = Some(
                    element
                        .active_playback_loop_pair
                        .unwrap_or(0)
                        .min(element.playback_loop_pairs.len() - 1),
                );
            } else {
                element.active_playback_loop_pair = None;
            }
        }
        scene.current_scale = if scene.current_scale.is_finite() {
            scene.current_scale.clamp(0.01, 2.0)
        } else {
            1.0
        };
        Ok(scene)
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut stable = self.clone();
        stable.format_version = Some(CURRENT_SCENE_VERSION);
        stable.mode = Some("standard".into());
        stable.edit_video = None;
        Ok(serde_json::to_string_pretty(&stable)?)
    }
}

fn valid_pair(pair: [f64; 2]) -> bool {
    pair[0].is_finite()
        && pair[1].is_finite()
        && pair[0] >= 0.0
        && pair[1] <= 100.0
        && pair[0] < pair[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"{
      "mode":"standard","currentScale":1.25,
      "translate":{"translateX":-12,"translateY":7},
      "workspaceRect":{"x1":0,"y1":0,"x2":640,"y2":360},
      "elements":[
        {"path":"C:\\refs\\still.png","type":"img","x":4,"y":9,"width":120,"height":80},
        {"path":"https://www.youtube.com/watch?v=abc123","type":"youtube","x":10,"y":20,"width":640,"height":360,"loopPairs":[[15,42],[60,80]],"activeLoopPair":0},
        {"path":"hello","type":"text","x":0,"y":0,"width":200,"height":40,"unknownFutureField":true}
      ]
    }"#;

    #[test]
    fn reads_legacy_scenes_and_round_trips_known_fields() {
        let scene = Scene::from_json(LEGACY).unwrap();
        assert_eq!(scene.current_scale, 1.25);
        assert_eq!(scene.translate.x, -12.0);
        assert_eq!(scene.elements.len(), 3);
        assert_eq!(scene.elements[1].loop_pairs[0], [15.0, 42.0]);
        assert_eq!(scene.elements[0].playback_state, PlaybackState::default());
        assert_eq!(scene.grid, GridSettings::default());
        assert_eq!(scene.format_version, Some(3));
        let output = scene.to_json().unwrap();
        assert!(output.contains("\"formatVersion\""));
        assert!(!output.contains("\"format_version\""));
        let reread = Scene::from_json(&output).unwrap();
        assert_eq!(reread.elements[1].path, scene.elements[1].path);
        assert_eq!(reread.elements[1].loop_pairs, scene.elements[1].loop_pairs);
        assert!(!output.contains("unknownFutureField"));
    }

    #[test]
    fn persists_playback_and_grid_settings_in_v3_scenes() {
        let mut scene = Scene {
            grid: GridSettings {
                enabled: true,
                spacing: 48.0,
            },
            ..Scene::default()
        };
        let mut element = Element::new("clip.gif".into(), "gif", 0.0, 0.0, 80.0, 80.0);
        element.playback_state = PlaybackState {
            paused: true,
            position_seconds: 2.35,
        };
        scene.elements.push(element);

        let json = scene.to_json().unwrap();
        assert!(json.contains("\"formatVersion\": 3"));
        assert!(json.contains("\"grid\""));
        assert!(json.contains("\"playbackState\""));
        let reread = Scene::from_json(&json).unwrap();
        assert!(reread.grid.enabled);
        assert_eq!(reread.grid.spacing, 48.0);
        assert_eq!(
            reread.elements[0].playback_state,
            PlaybackState {
                paused: true,
                position_seconds: 2.35
            }
        );
    }

    #[test]
    fn malformed_optional_values_are_repaired() {
        let scene = Scene::from_json(r#"{"currentScale":99,"grid":{"enabled":true,"spacing":-1},"elements":[{"type":"video","width":-2,"playbackState":{"paused":true,"position_seconds":-4},"loopPairs":[[70,20]],"activeLoopPair":99}]}"#).unwrap();
        assert_eq!(scene.current_scale, 2.0);
        assert_eq!(scene.elements[0].width, None);
        assert_eq!(scene.elements[0].loop_pairs, vec![[0.0, 100.0]]);
        assert_eq!(scene.elements[0].active_loop_pair, 0);
        assert_eq!(scene.grid.spacing, 32.0);
        assert_eq!(scene.elements[0].playback_state.position_seconds, 0.0);
    }

    #[test]
    fn repairs_legacy_null_geometry_from_json_nan_values() {
        let scene = Scene::from_json(
            r#"{
              "currentScale":null,
              "translate":{"translateX":null,"translateY":null},
              "workspaceRect":{"x1":0,"y1":0,"x2":null,"y2":null},
              "elements":[
                {"path":"portrait.mp4","type":"video","x":12,"y":24,"width":null,"height":null},
                {"path":"remote-a","type":"filePath","x":1,"y":2,"width":null,"height":null},
                {"path":"remote-b","type":"filePath","x":3,"y":4,"width":null,"height":null},
                {"path":"still.png","type":"img","x":5,"y":6,"width":null,"height":null},
                {"path":"caption","type":"text","x":7,"y":8,"width":null,"height":null},
                {"path":"clip.mp4","type":"video","x":9,"y":10,"width":null,"height":null},
                {"path":"other.png","type":"img","x":11,"y":12,"width":null,"height":null}
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(scene.current_scale, 1.0);
        assert_eq!(scene.translate.x, 0.0);
        assert_eq!(scene.translate.y, 0.0);
        assert_eq!(scene.workspace_rect.x1, 0.0);
        assert_eq!(scene.workspace_rect.y1, 0.0);
        assert_eq!(scene.workspace_rect.x2, 0.0);
        assert_eq!(scene.workspace_rect.y2, 0.0);
        assert_eq!(scene.elements.len(), 7);
        assert_eq!((scene.elements[0].x, scene.elements[0].y), (12.0, 24.0));
        assert_eq!(scene.elements[0].width, None);
        assert_eq!(scene.elements[0].height, None);
    }

    #[test]
    fn migrates_legacy_youtube_urls_mislabeled_as_images() {
        let scene = Scene::from_json(
            r#"{"elements":[
              {"type":"filePath","path":"https://youtu.be/bmuBzhuuqno?feature=shared","x":10,"y":20,"width":640,"height":360,"loopPairs":[[25,75]],"activeLoopPair":0},
              {"type":"img","path":"https://images.example.org/picture.jpg","x":1,"y":2},
              {"type":"filePath","path":"https://images.example.org/legacy-image","x":3,"y":4}
            ]}"#,
        )
        .unwrap();

        assert_eq!(scene.elements[0].media_type, "youtube");
        assert_eq!(
            scene.elements[0].path,
            "https://youtu.be/bmuBzhuuqno?feature=shared"
        );
        assert_eq!(scene.elements[0].loop_pairs, vec![[25.0, 75.0]]);
        assert_eq!(scene.elements[0].width, Some(640.0));
        assert_eq!(scene.elements[0].height, Some(360.0));
        assert_eq!(scene.elements[1].media_type, "img");
        assert_eq!(scene.elements[2].media_type, "filePath");
    }

    #[test]
    fn legacy_youtube_source_is_preserved_explicitly_without_touching_source_timing() {
        let scene = Scene::from_json(
            r#"{"elements":[{"type":"youtube","path":"https://youtu.be/bmuBzhuuqno?feature=shared","sourceRange":[1.5,4.0],"sourceDuration":12.0}]}"#,
        )
        .unwrap();
        let element = &scene.elements[0];
        assert_eq!(
            element.source_link(),
            Some("https://youtu.be/bmuBzhuuqno?feature=shared")
        );
        assert_eq!(element.source_range, Some([1.5, 4.0]));
        assert_eq!(element.source_duration, Some(12.0));
        let json = scene.to_json().unwrap();
        assert!(json.contains("\"sourceUrl\": \"https://youtu.be/bmuBzhuuqno?feature=shared\""));
        let reread = Scene::from_json(&json).unwrap();
        assert_eq!(reread.elements[0].source_url, element.source_url);
        assert_eq!(reread.elements[0].source_range, element.source_range);
        assert_eq!(reread.elements[0].source_duration, element.source_duration);
    }

    #[test]
    fn source_link_rejects_unsafe_urls_but_round_trips_pinterest_source() {
        let mut scene = Scene::default();
        let mut element = Element::new("pin.jpg".into(), "img", 0.0, 0.0, 100.0, 80.0);
        element.source_url = Some("https://www.pinterest.com/pin/123456789/".into());
        scene.elements.push(element);
        let json = scene.to_json().unwrap();
        assert!(json.contains("\"sourceUrl\": \"https://www.pinterest.com/pin/123456789/\""));
        let reread = Scene::from_json(&json).unwrap();
        assert_eq!(
            reread.elements[0].source_link(),
            Some("https://www.pinterest.com/pin/123456789/")
        );

        let mut unsafe_element = reread.elements[0].clone();
        unsafe_element.source_url = Some("javascript:alert(1)".into());
        assert_eq!(unsafe_element.source_link(), None);
        unsafe_element.source_url = Some("https://user@example.com/pin".into());
        assert_eq!(unsafe_element.source_link(), None);
        unsafe_element.source_url = Some("https://example.com:bad/pin".into());
        assert_eq!(unsafe_element.source_link(), None);
    }

    #[test]
    fn legacy_youtube_range_maps_to_source_time() {
        let mut scene = Scene::from_json(LEGACY).unwrap();
        let youtube = &mut scene.elements[1];
        youtube.source_duration = Some(100.0);
        assert_eq!(youtube.source_download_range(), Some([15.0, 42.0]));
    }

    #[test]
    fn restores_camera_from_a_legacy_video_edit_scene() {
        let scene = Scene::from_json(
            r#"{"mode":"edit-video","currentScale":1,"translate":{"translateX":0,"translateY":0},"editVideo":{"backupState":{"currentScale":1.7,"translate":{"translateX":23,"translateY":-8},"elementClasses":"editVideo"}},"elements":[]}"#,
        ).unwrap();
        assert_eq!(scene.current_scale, 1.7);
        assert_eq!(scene.translate.x, 23.0);
        assert_eq!(scene.translate.y, -8.0);
        assert_eq!(scene.mode.as_deref(), Some("standard"));
        assert!(scene.edit_video.is_none());
    }
}
