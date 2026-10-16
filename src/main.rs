#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod media;
mod model;
mod youtube;

use anyhow::{Context, Result};
use base64::Engine;
#[cfg(windows)]
use clipboard_win::Getter;
use eframe::egui::{
    self, Color32, ColorImage, Context as EguiContext, FontId, Key, Pos2, Rect, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2, ViewportCommand, WindowLevel,
};
use image::{AnimationDecoder, GenericImageView};
use model::{Element, Scene};
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use youtube::{DownloadEvent, DownloadJob, VideoMetadata};

type RemoteImageSuccess = (u64, Option<u64>, String, PathBuf);
type RemoteImageFailure = (u64, Option<u64>, String);
type RemoteImageResult = Result<RemoteImageSuccess, RemoteImageFailure>;

struct CompletedYoutubeDownload {
    path: PathBuf,
    metadata: VideoMetadata,
    url: String,
    target_element: Option<u64>,
    generation: u64,
    start_text: String,
    end_text: String,
}

fn main() -> eframe::Result<()> {
    let cli = CliOptions::parse(std::env::args_os().skip(1));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 650.0])
            .with_min_inner_size([480.0, 320.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_title("AnimRef"),
        ..Default::default()
    };
    eframe::run_native(
        "AnimRef",
        options,
        Box::new(|cc| {
            cc.egui_ctx
                .send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
            Ok(Box::new(AnimRefApp::new(cc, cli)))
        }),
    )
}

#[derive(Default)]
struct CliOptions {
    scene_path: Option<PathBuf>,
    screenshot_path: Option<PathBuf>,
    smoke_report_path: Option<PathBuf>,
    quit_after_smoke: bool,
}

impl CliOptions {
    fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Self {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--quit-after-smoke" {
                options.quit_after_smoke = true;
                continue;
            }
            let value = match arg.to_string_lossy().as_ref() {
                "--scene" => &mut options.scene_path,
                "--screenshot" => &mut options.screenshot_path,
                "--smoke-report" => &mut options.smoke_report_path,
                _ => continue,
            };
            if let Some(path) = args.next() {
                *value = Some(PathBuf::from(path));
            }
        }
        options
    }
}

struct GifAsset {
    frames: Vec<(TextureHandle, Duration)>,
    started: Instant,
    total_duration: Duration,
}

struct YoutubeDialog {
    url: String,
    metadata: Option<VideoMetadata>,
    metadata_waiting: bool,
    metadata_job: Option<youtube::MetadataJob>,
    start_text: String,
    end_text: String,
    progress: f32,
    job: Option<DownloadJob>,
    error: Option<String>,
    target_element: Option<u64>,
    generation: u64,
}

impl YoutubeDialog {
    fn new(url: String, target_element: Option<u64>, generation: u64) -> Self {
        Self {
            url,
            metadata: None,
            metadata_waiting: false,
            metadata_job: None,
            start_text: "0".into(),
            end_text: String::new(),
            progress: 0.0,
            job: None,
            error: None,
            target_element,
            generation,
        }
    }
}

struct AnimRefApp {
    scene: Scene,
    selected: Option<usize>,
    scene_path: Option<PathBuf>,
    recent: Vec<PathBuf>,
    status: String,
    always_on_top: bool,
    show_help: bool,
    show_youtube: Option<YoutubeDialog>,
    show_text_dialog: bool,
    text_draft: String,
    image_assets: HashMap<String, TextureHandle>,
    gif_assets: HashMap<String, GifAsset>,
    image_errors: HashMap<String, String>,
    video_players: HashMap<String, media::VideoPlayer>,
    video_textures: HashMap<String, TextureHandle>,
    video_errors: HashMap<String, String>,
    pending_video_seek: HashMap<u64, f64>,
    remote_jobs: Vec<crossbeam_channel::Receiver<RemoteImageResult>>,
    remote_pending: HashSet<u64>,
    scene_generation: u64,
    edit_video_id: Option<u64>,
    edit_view_backup: Option<(f32, model::Translate)>,
    cli: CliOptions,
    started_at: Instant,
    update_count: u64,
    decoded_video_frames: u64,
    decoded_video_frames_by_id: HashMap<u64, u64>,
    screenshot_requested: bool,
    screenshot_result: Option<String>,
    smoke_report_written: bool,
    smoke_report_update: Option<u64>,
    diagnostic_input_events: VecDeque<String>,
    diagnostic_modifiers: String,
    diagnostic_wants_keyboard: bool,
}

impl AnimRefApp {
    fn new(cc: &eframe::CreationContext<'_>, cli: CliOptions) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        install_cjk_font(&cc.egui_ctx);
        let recent = load_recent();
        let mut app = Self {
            scene: Scene::default(),
            selected: None,
            scene_path: None,
            recent,
            status: "Drop images, GIFs, or videos here. Paste a YouTube URL to choose a clip."
                .into(),
            always_on_top: true,
            show_help: false,
            show_youtube: None,
            show_text_dialog: false,
            text_draft: String::new(),
            image_assets: HashMap::new(),
            gif_assets: HashMap::new(),
            image_errors: HashMap::new(),
            video_players: HashMap::new(),
            video_textures: HashMap::new(),
            video_errors: HashMap::new(),
            pending_video_seek: HashMap::new(),
            remote_jobs: Vec::new(),
            remote_pending: HashSet::new(),
            scene_generation: 1,
            edit_video_id: None,
            edit_view_backup: None,
            cli,
            started_at: Instant::now(),
            update_count: 0,
            decoded_video_frames: 0,
            decoded_video_frames_by_id: HashMap::new(),
            screenshot_requested: false,
            screenshot_result: None,
            smoke_report_written: false,
            smoke_report_update: None,
            diagnostic_input_events: VecDeque::with_capacity(32),
            diagnostic_modifiers: String::new(),
            diagnostic_wants_keyboard: false,
        };
        if let Some(scene_path) = app.cli.scene_path.clone() {
            if let Err(error) = app.load_scene(&scene_path) {
                app.status = format!("Could not load {}: {error:#}", scene_path.display());
                eprintln!("AnimRef smoke scene load failed: {error:#}");
            }
        } else if let Some(last) = app.recent.last().cloned() {
            let _ = app.load_scene(&last);
        }
        app
    }

    fn import_path(&mut self, path: &Path) {
        let Some(path_text) = path.to_str().map(str::to_owned) else {
            return;
        };
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let center = Pos2::new(
            220.0 + self.scene.elements.len() as f32 * 16.0,
            160.0 + self.scene.elements.len() as f32 * 16.0,
        );
        if matches!(
            extension.as_str(),
            "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi"
        ) {
            let duration = media::probe_duration(path).ok();
            let mut element = Element::new(path_text, "video", center.x, center.y, 640.0, 360.0);
            if let Ok((width, height)) = media::probe_dimensions(path) {
                element.height = Some(640.0 * height as f32 / width.max(1) as f32);
            }
            element.media_duration = duration;
            self.scene.elements.push(element);
        } else {
            match image::open(path) {
                Ok(image) => {
                    let size = image.dimensions();
                    let mut element = Element::new(
                        path_text,
                        "img",
                        center.x,
                        center.y,
                        size.0 as f32,
                        size.1 as f32,
                    );
                    element.media_duration = None;
                    self.scene.elements.push(element);
                }
                Err(error) => self.status = format!("Could not open {}: {error}", path.display()),
            }
        }
        self.selected = self.scene.elements.len().checked_sub(1);
    }

    fn load_scene(&mut self, path: &Path) -> Result<()> {
        self.show_youtube = None;
        self.scene_generation = self.scene_generation.wrapping_add(1);
        let scene_path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let text = fs::read_to_string(path)
            .with_context(|| format!("Could not read {}", path.display()))?;
        let mut scene = Scene::from_json(&text).context("The scene file is not valid JSON")?;
        let parent = scene_path.parent().unwrap_or(Path::new("."));
        resolve_scene_asset_paths(&mut scene, parent);
        for element in &mut scene.elements {
            if element.media_type == "video" && element.media_duration.is_none() {
                element.media_duration = media::probe_duration(Path::new(&element.path)).ok();
            }
            if element.is_video()
                && (element.width.is_none() || element.height.is_none())
                && let Some(path) = element
                    .display_path()
                    .filter(|path| !is_http_url(&path.to_string_lossy()))
                && let Ok((width, height)) = media::probe_dimensions(&path)
            {
                fill_missing_dimensions(element, width as f32, height as f32);
            }
        }
        self.scene = scene;
        self.scene_path = Some(scene_path.clone());
        self.selected = None;
        self.edit_video_id = None;
        self.edit_view_backup = None;
        self.clear_media_caches();
        self.add_recent(scene_path.clone());
        self.status = format!("Loaded {}", scene_path.display());
        Ok(())
    }

    fn save_scene(&mut self) -> Result<()> {
        let path = if let Some(path) = self.scene_path.clone() {
            path
        } else {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("AnimRef scene", &["purgif"])
                .set_file_name("scene.purgif")
                .save_file()
            else {
                return Ok(());
            };
            path
        };
        self.save_scene_to(path)
    }

    fn save_scene_as(&mut self) -> Result<()> {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("AnimRef scene", &["purgif"])
            .set_file_name("scene.purgif")
            .save_file()
        else {
            return Ok(());
        };
        self.save_scene_to(path)
    }

    fn save_scene_to(&mut self, path: PathBuf) -> Result<()> {
        let mut scene_to_save = self.scene.clone();
        if let Some((scale, translate)) = self.edit_view_backup {
            scene_to_save.current_scale = scale;
            scene_to_save.translate = translate;
        }
        let text = scene_to_save.to_json()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("Could not stage save beside {}", path.display()))?;
        std::io::Write::write_all(&mut temporary, text.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("Could not replace {}", path.display()))?;
        self.scene_path = Some(path.clone());
        self.add_recent(path.clone());
        self.status = format!("Saved {}", path.display());
        Ok(())
    }

    fn add_recent(&mut self, path: PathBuf) {
        self.recent.retain(|entry| entry != &path);
        self.recent.push(path);
        if self.recent.len() > 12 {
            self.recent.drain(..self.recent.len() - 12);
        }
        store_recent(&self.recent);
    }

    fn new_scene(&mut self) {
        self.show_youtube = None;
        self.scene_generation = self.scene_generation.wrapping_add(1);
        self.scene = Scene::default();
        self.scene_path = None;
        self.selected = None;
        self.edit_video_id = None;
        self.edit_view_backup = None;
        self.clear_media_caches();
        self.status = "New scene".into();
    }

    fn clear_media_caches(&mut self) {
        self.video_players.clear();
        self.video_textures.clear();
        self.video_errors.clear();
        self.image_assets.clear();
        self.gif_assets.clear();
        self.image_errors.clear();
        self.pending_video_seek.clear();
    }

    fn clear_item_cache(&mut self, id: u64) {
        let key = id.to_string();
        self.video_players.remove(&key);
        self.video_textures.remove(&key);
        self.video_errors.remove(&key);
        self.image_assets.remove(&key);
        self.gif_assets.remove(&key);
        self.image_errors.remove(&key);
        self.pending_video_seek.remove(&id);
        self.remote_pending.remove(&id);
    }

    fn enter_video_edit(&mut self, id: u64) {
        if self.edit_video_id.is_some() {
            self.exit_video_edit();
        }
        self.edit_view_backup = Some((self.scene.current_scale, self.scene.translate));
        self.scene.current_scale = 1.0;
        self.scene.translate = model::Translate::default();
        self.edit_video_id = Some(id);
    }

    fn exit_video_edit(&mut self) {
        self.edit_video_id = None;
        if let Some((scale, translate)) = self.edit_view_backup.take() {
            self.scene.current_scale = scale;
            self.scene.translate = translate;
        }
    }

    fn start_youtube_metadata(&mut self, url: String, target_element: Option<u64>) {
        let mut dialog = YoutubeDialog::new(url.clone(), target_element, self.scene_generation);
        match youtube::begin_metadata(url) {
            Ok(job) => {
                dialog.metadata_waiting = true;
                dialog.metadata_job = Some(job);
            }
            Err(error) => dialog.error = Some(error.to_string()),
        }
        self.show_youtube = Some(dialog);
    }

    fn add_text(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let element = Element::new(text.clone(), "text", 160.0, 140.0, 360.0, 70.0);
        self.scene.elements.push(element);
        self.selected = self.scene.elements.len().checked_sub(1);
    }

    fn handle_paste(&mut self) {
        #[cfg(windows)]
        {
            let mut files = Vec::<String>::new();
            if clipboard_win::formats::FileList
                .read_clipboard(&mut files)
                .is_ok()
                && !files.is_empty()
            {
                for path in files {
                    self.import_path(Path::new(&path));
                }
                return;
            }
        }
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if let Ok(text) = clipboard.get_text() {
                let text = text.trim().to_owned();
                if is_youtube_url(&text) {
                    self.start_youtube_metadata(text, None);
                    return;
                }
                if let Some(path) = clipboard_path(&text) {
                    self.import_path(&path);
                    return;
                }
                if text.starts_with("http://") || text.starts_with("https://") {
                    self.import_remote_url(text);
                    return;
                }
                if !text.is_empty() {
                    self.add_text(text);
                    return;
                }
            }
            if let Ok(image) = clipboard.get_image() {
                let directory = youtube::cache_dir().join("pasted");
                let _ = fs::create_dir_all(&directory);
                let path = directory.join(format!(
                    "clipboard-{}.png",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                ));
                match image::save_buffer(
                    &path,
                    &image.bytes,
                    image.width as u32,
                    image.height as u32,
                    image::ExtendedColorType::Rgba8,
                ) {
                    Ok(()) => self.import_path(&path),
                    Err(error) => self.status = format!("Could not save clipboard image: {error}"),
                }
                return;
            }
        }
        self.status = "Clipboard is empty or uses an unsupported format".into();
    }

    fn import_remote_url(&mut self, url: String) {
        let extension = url
            .split(['?', '#'])
            .next()
            .unwrap_or(&url)
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            extension.as_str(),
            "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi"
        ) {
            let element = Element::new(url, "video", 180.0, 130.0, 640.0, 360.0);
            self.scene.elements.push(element);
            self.selected = self.scene.elements.len().checked_sub(1);
            self.status = "Streaming remote video".into();
        } else {
            let mut element = Element::new(url.clone(), "img", 180.0, 130.0, 480.0, 320.0);
            element.cached_path = None;
            self.scene.elements.push(element);
            let id = self.scene.elements.last().map(|e| e.id).unwrap_or_default();
            self.selected = self.scene.elements.len().checked_sub(1);
            self.schedule_remote_image(url, Some(id));
        }
    }

    fn schedule_remote_image(&mut self, url: String, target_id: Option<u64>) {
        if let Some(id) = target_id
            && !self.remote_pending.insert(id)
        {
            return;
        }
        let generation = self.scene_generation;
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.remote_jobs.push(rx);
        self.status = "Downloading remote image…".into();
        std::thread::spawn(move || {
            let result = (|| -> Result<PathBuf> {
                const MAX_REMOTE_IMAGE_BYTES: u64 = 40 * 1024 * 1024;
                let response = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(25))
                    .build()?
                    .get(&url)
                    .send()?
                    .error_for_status()?;
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if content_type.starts_with("video/") {
                    anyhow::bail!(
                        "URL response is video content; only image URLs can be imported here"
                    );
                }
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_REMOTE_IMAGE_BYTES)
                {
                    anyhow::bail!("Remote image is larger than 40 MiB");
                }
                let mut bytes = Vec::new();
                response
                    .take(MAX_REMOTE_IMAGE_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_REMOTE_IMAGE_BYTES {
                    anyhow::bail!("Remote image is larger than 40 MiB");
                }
                let format = image::guess_format(&bytes)
                    .context("URL response is not a recognized image")?;
                image::load_from_memory(&bytes).context("URL response is not a valid image")?;
                let extension = format
                    .extensions_str()
                    .first()
                    .copied()
                    .context("Image format has no file extension")?;
                let directory = youtube::cache_dir().join("remote");
                fs::create_dir_all(&directory)?;
                let path = directory.join(format!(
                    "remote-{}.{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                    extension
                ));
                fs::write(&path, bytes)?;
                Ok(path)
            })()
            .map(|path| (generation, target_id, url, path))
            .map_err(|error| {
                (
                    generation,
                    target_id,
                    format!("Could not download remote image: {error:#}"),
                )
            });
            let _ = tx.send(result);
        });
    }

    fn drain_remote_imports(&mut self) {
        let mut pending = Vec::new();
        let jobs = std::mem::take(&mut self.remote_jobs);
        for job in jobs {
            match job.try_recv() {
                Ok(Ok((generation, target, _url, path))) if generation == self.scene_generation => {
                    if let Some(id) = target {
                        self.remote_pending.remove(&id);
                        if let Some(element) = self.scene.elements.iter_mut().find(|e| e.id == id) {
                            element.cached_path = Some(path);
                            self.status = "Remote image loaded".into();
                        }
                    } else {
                        self.import_path(&path);
                        self.status = "Remote image added".into();
                    }
                }
                Ok(Ok((_, Some(id), _, _))) => {
                    self.remote_pending.remove(&id);
                }
                Ok(Ok(_)) => (),
                Ok(Err((generation, target, error))) => {
                    if let Some(id) = target {
                        self.remote_pending.remove(&id);
                        if generation == self.scene_generation {
                            self.image_errors.insert(id.to_string(), error.clone());
                        }
                    }
                    if generation == self.scene_generation {
                        self.status = error;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => pending.push(job),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.status = "Remote image download stopped unexpectedly".into()
                }
            }
        }
        self.remote_jobs = pending;
    }

    fn ensure_assets(&mut self, ctx: &EguiContext) {
        let remote_to_fetch: Vec<_> = self
            .scene
            .elements
            .iter()
            .filter(|e| {
                matches!(e.media_type.as_str(), "img" | "filePath" | "dataURL")
                    && is_http_url(&e.path)
                    && !remote_url_is_video(&e.path)
                    && e.cached_path.as_ref().is_none_or(|p| !p.exists())
                    && !self.remote_pending.contains(&e.id)
                    && !self.image_errors.contains_key(&e.id.to_string())
            })
            .map(|e| (e.id, e.path.clone()))
            .collect();
        for (id, url) in remote_to_fetch {
            self.schedule_remote_image(url, Some(id));
        }
        let mut intrinsic_sizes = Vec::new();
        for element in &self.scene.elements {
            if element.media_type == "text" || element.is_video() {
                continue;
            }
            let key = element.id.to_string();
            if self.image_assets.contains_key(&key)
                || self.gif_assets.contains_key(&key)
                || self.image_errors.contains_key(&key)
            {
                continue;
            }
            let source = element
                .cached_path
                .as_ref()
                .filter(|p| p.exists())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| element.path.clone());
            if is_http_url(&source) {
                continue;
            }
            match read_image_source(&source) {
                Ok(bytes) => {
                    if is_gif_source(&source, &bytes) {
                        match image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
                            .and_then(|decoder| decoder.into_frames().collect_frames())
                        {
                            Ok(frames) => {
                                let intrinsic_size =
                                    frames.first().map(|frame| frame.buffer().dimensions());
                                let mut rendered = Vec::new();
                                for frame in frames {
                                    let delay = frame.delay().numer_denom_ms();
                                    let millis = delay.0.checked_div(delay.1).unwrap_or(80) as u64;
                                    let buffer = frame.into_buffer();
                                    let image = ColorImage::from_rgba_unmultiplied(
                                        [buffer.width() as usize, buffer.height() as usize],
                                        buffer.as_raw(),
                                    );
                                    let texture = ctx.load_texture(
                                        format!("{key}-{}", rendered.len()),
                                        image,
                                        TextureOptions::LINEAR,
                                    );
                                    rendered.push((texture, Duration::from_millis(millis.max(20))));
                                }
                                if !rendered.is_empty() {
                                    if let Some((width, height)) = intrinsic_size {
                                        intrinsic_sizes.push((
                                            element.id,
                                            width as f32,
                                            height as f32,
                                        ));
                                    }
                                    let total = rendered.iter().map(|(_, d)| *d).sum();
                                    self.gif_assets.insert(
                                        key,
                                        GifAsset {
                                            frames: rendered,
                                            started: Instant::now(),
                                            total_duration: total,
                                        },
                                    );
                                }
                            }
                            Err(error) => {
                                self.image_errors.insert(key, error.to_string());
                            }
                        }
                    } else {
                        match image::load_from_memory(&bytes) {
                            Ok(dynamic) => {
                                let rgba = dynamic.to_rgba8();
                                intrinsic_sizes.push((
                                    element.id,
                                    rgba.width() as f32,
                                    rgba.height() as f32,
                                ));
                                let image = ColorImage::from_rgba_unmultiplied(
                                    [rgba.width() as usize, rgba.height() as usize],
                                    rgba.as_raw(),
                                );
                                self.image_assets.insert(
                                    key.clone(),
                                    ctx.load_texture(key, image, TextureOptions::LINEAR),
                                );
                            }
                            Err(error) => {
                                self.image_errors.insert(key, error.to_string());
                            }
                        }
                    }
                }
                Err(error) => {
                    self.image_errors.insert(key, error.to_string());
                }
            }
        }
        for (id, source_width, source_height) in intrinsic_sizes {
            if let Some(element) = self
                .scene
                .elements
                .iter_mut()
                .find(|element| element.id == id)
            {
                fill_missing_dimensions(element, source_width, source_height);
            }
        }
        self.start_video_players();
        self.drain_video_frames(ctx);
    }

    fn start_video_players(&mut self) {
        for element in &self.scene.elements {
            if !element.is_video() {
                continue;
            }
            let Some(path) = element.display_path() else {
                continue;
            };
            let key = element.id.to_string();
            if self.video_players.contains_key(&key) || self.video_errors.contains_key(&key) {
                continue;
            }
            let is_downloaded_youtube = element.media_type == "youtube";
            let duration = if is_downloaded_youtube {
                element
                    .media_duration
                    .or_else(|| element.source_range.map(|r| r[1] - r[0]))
            } else {
                element.media_duration
            };
            let pairs = if is_downloaded_youtube && !element.playback_loop_pairs.is_empty() {
                element.playback_loop_pairs.clone()
            } else {
                element.loop_pairs.clone()
            };
            let active = if is_downloaded_youtube {
                element.active_playback_pair()
            } else {
                element.active_loop_pair
            };
            let seek = self.pending_video_seek.remove(&element.id);
            self.video_players.insert(
                key,
                media::VideoPlayer::start_at(path, duration, pairs, active, seek),
            );
        }
    }

    fn drain_video_frames(&mut self, ctx: &EguiContext) {
        let mut received_frame = false;
        for (key, player) in &self.video_players {
            while let Ok(event) = player.events.try_recv() {
                match event {
                    media::PlayerEvent::Frame(bytes) => {
                        if let Ok(image) = image::load_from_memory(&bytes) {
                            self.decoded_video_frames = self.decoded_video_frames.saturating_add(1);
                            if let Ok(element_id) = key.parse::<u64>() {
                                let count = self
                                    .decoded_video_frames_by_id
                                    .entry(element_id)
                                    .or_default();
                                *count = count.saturating_add(1);
                            }
                            received_frame = true;
                            let rgba = image.to_rgba8();
                            let color = ColorImage::from_rgba_unmultiplied(
                                [rgba.width() as usize, rgba.height() as usize],
                                rgba.as_raw(),
                            );
                            if let Some(texture) = self.video_textures.get_mut(key) {
                                texture.set(color, TextureOptions::LINEAR);
                            } else {
                                self.video_textures.insert(
                                    key.clone(),
                                    ctx.load_texture(key.clone(), color, TextureOptions::LINEAR),
                                );
                            }
                        }
                    }
                    media::PlayerEvent::Failed(error) => {
                        self.video_errors.insert(key.clone(), error);
                    }
                }
            }
        }
        if received_frame && self.smoke_report_written {
            self.write_smoke_report();
        }
    }

    fn update_diagnostics(&mut self, ctx: &EguiContext) {
        self.update_count = self.update_count.saturating_add(1);
        let (input_events, modifiers) = ctx.input(|input| {
            (
                input
                    .events
                    .iter()
                    .map(summarize_input_event)
                    .collect::<Vec<_>>(),
                format!("{:?}", input.modifiers),
            )
        });
        let had_input_events = !input_events.is_empty();
        self.diagnostic_wants_keyboard = ctx.wants_keyboard_input();
        self.diagnostic_modifiers = modifiers;
        for event in input_events {
            if self.diagnostic_input_events.len() == 32 {
                self.diagnostic_input_events.pop_front();
            }
            self.diagnostic_input_events.push_back(event);
        }
        let screenshot_events = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let got_screenshot = !screenshot_events.is_empty();
        for image in screenshot_events {
            if let Some(path) = self.cli.screenshot_path.as_ref() {
                if let Some(parent) = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    && let Err(error) = fs::create_dir_all(parent)
                {
                    self.screenshot_result = Some(format!(
                        "Could not create screenshot directory {}: {error}",
                        parent.display()
                    ));
                    continue;
                }
                let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
                for pixel in &image.pixels {
                    rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
                }
                let result = image::save_buffer(
                    path,
                    &rgba,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .map(|()| {
                    format!(
                        "saved {}x{} rendered GPU frame to {}",
                        image.size[0],
                        image.size[1],
                        path.display()
                    )
                })
                .map_err(|error| format!("Could not save screenshot {}: {error}", path.display()));
                self.screenshot_result = Some(match result {
                    Ok(message) => message,
                    Err(error) => {
                        eprintln!("AnimRef screenshot failed: {error}");
                        error
                    }
                });
            }
        }
        if got_screenshot
            && self.cli.smoke_report_path.is_some()
            && self.started_at.elapsed() >= Duration::from_secs(2)
        {
            self.write_smoke_report();
        }

        let video_textures_ready = self
            .scene
            .elements
            .iter()
            .filter(|element| element.is_video())
            .all(|element| self.video_textures.contains_key(&element.id.to_string()));
        if !self.screenshot_requested
            && self.cli.screenshot_path.is_some()
            && self.started_at.elapsed() >= Duration::from_secs(2)
            && (video_textures_ready || self.started_at.elapsed() >= Duration::from_secs(10))
        {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            ctx.request_repaint();
        }

        if self.smoke_report_written && had_input_events {
            self.write_smoke_report();
        }

        if !self.smoke_report_written
            && self.cli.smoke_report_path.is_some()
            && self.started_at.elapsed() >= Duration::from_secs(2)
            && (self.cli.screenshot_path.is_none()
                || self.screenshot_result.is_some()
                || self.started_at.elapsed() >= Duration::from_secs(12))
        {
            self.write_smoke_report();
        }

        let screenshot_succeeded = self.cli.screenshot_path.is_none()
            || self
                .screenshot_result
                .as_ref()
                .is_some_and(|result| result.starts_with("saved "));
        if self.cli.quit_after_smoke
            && self.smoke_report_written
            && screenshot_succeeded
            && self
                .smoke_report_update
                .is_some_and(|written_at| self.update_count >= written_at.saturating_add(3))
        {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn write_smoke_report(&mut self) {
        let Some(report_path) = self.cli.smoke_report_path.as_ref() else {
            return;
        };
        let elements = self
            .scene
            .elements
            .iter()
            .map(|element| {
                let key = element.id.to_string();
                serde_json::json!({
                    "id": element.id,
                    "type": element.media_type,
                    "path": element.display_path(),
                    "x": element.x,
                    "y": element.y,
                    "width": element.width,
                    "height": element.height,
                    "sourceRange": element.source_range,
                    "mediaDuration": element.media_duration,
                    "decodedFrames": self.decoded_video_frames_by_id.get(&element.id).copied().unwrap_or(0),
                    "playheadSeconds": self.video_players.get(&key).map(media::VideoPlayer::playhead),
                    "imageError": self.image_errors.get(&key),
                    "videoError": self.video_errors.get(&key),
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "processId": std::process::id(),
            "scenePath": self.scene_path,
            "elapsedSeconds": self.started_at.elapsed().as_secs_f64(),
            "updateCount": self.update_count,
            "firstUpdateObserved": self.update_count > 0,
            "screenshotRequested": self.screenshot_requested,
            "screenshotResult": self.screenshot_result,
            "decodedVideoFrames": self.decoded_video_frames,
            "lastInputEvents": self.diagnostic_input_events,
            "modifiers": self.diagnostic_modifiers,
            "wantsKeyboardInput": self.diagnostic_wants_keyboard,
            "elements": elements,
            "status": self.status,
        });
        let result = serde_json::to_vec_pretty(&report)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                if let Some(parent) = report_path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    fs::create_dir_all(parent)?;
                }
                fs::write(report_path, bytes)
                    .with_context(|| format!("Could not write {}", report_path.display()))
            });
        if let Err(error) = result {
            eprintln!("AnimRef smoke report failed: {error:#}");
        } else {
            self.smoke_report_written = true;
            self.smoke_report_update = Some(self.update_count);
        }
    }

    fn draw_top_bar(&mut self, ctx: &EguiContext) {
        egui::TopBottomPanel::top("titlebar")
            .exact_height(34.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(36, 36, 40)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let drag =
                        ui.add_sized([18.0, 28.0], egui::Label::new("⋮").sense(Sense::drag()));
                    if drag.dragged() {
                        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                    }
                    ui.label(
                        egui::RichText::new("AnimRef")
                            .strong()
                            .color(Color32::LIGHT_GRAY),
                    );
                    ui.separator();
                    if ui.button("Open").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("AnimRef scene", &["purgif"])
                            .pick_file()
                    {
                        let result = self.load_scene(&path);
                        self.capture_result(result);
                    }
                    if ui.button("Save").clicked() {
                        let result = self.save_scene();
                        self.capture_result(result);
                    }
                    if ui.button("Save As").clicked() {
                        let result = self.save_scene_as();
                        self.capture_result(result);
                    }
                    if ui.button("New").clicked() {
                        self.new_scene();
                    }
                    if ui.button("Add media").clicked()
                        && let Some(paths) = rfd::FileDialog::new()
                            .add_filter(
                                "Images and video",
                                &[
                                    "png", "jpg", "jpeg", "gif", "webp", "bmp", "mp4", "mov",
                                    "mkv", "webm", "avi",
                                ],
                            )
                            .pick_files()
                    {
                        for path in paths {
                            self.import_path(&path);
                        }
                    }
                    if ui.button("Text").clicked() {
                        self.show_text_dialog = true;
                    }
                    if ui.button("Paste").clicked() {
                        self.handle_paste();
                    }
                    ui.menu_button("Recent", |ui| {
                        for path in self.recent.clone().into_iter().rev() {
                            if ui.button(path.to_string_lossy()).clicked() {
                                let result = self.load_scene(&path);
                                self.capture_result(result);
                                ui.close();
                            }
                        }
                    });
                    if ui.button("Help").clicked() {
                        self.show_help = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        if ui.small_button("□").clicked() {
                            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        }
                        if ui.small_button("—").clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                        let mut always_on_top = self.always_on_top;
                        if ui.checkbox(&mut always_on_top, "Top").changed() {
                            self.always_on_top = always_on_top;
                            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if always_on_top {
                                WindowLevel::AlwaysOnTop
                            } else {
                                WindowLevel::Normal
                            }));
                        }
                    });
                });
            });
    }

    fn draw_inspector(&mut self, ctx: &EguiContext) {
        let mut youtube_action: Option<(String, u64)> = None;
        let mut edit_action: Option<u64> = None;
        let mut restart_video: Option<u64> = None;
        let mut delete_selected = false;
        let selected = self.selected.filter(|i| *i < self.scene.elements.len());
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(210.0)
            .min_width(170.0)
            .show(ctx, |ui| {
                ui.heading("Scene");
                ui.label(format!("{} items", self.scene.elements.len()));
                ui.horizontal(|ui| {
                    if ui.button("−").clicked() {
                        self.scene.current_scale = (self.scene.current_scale / 1.15).max(0.01);
                    }
                    ui.label(format!("{:.0}%", self.scene.current_scale * 100.0));
                    if ui.button("+").clicked() {
                        self.scene.current_scale = (self.scene.current_scale * 1.15).min(2.0);
                    }
                    if ui.button("Center").clicked() {
                        self.scene.translate = model::Translate::default();
                    }
                });
                ui.separator();
                let Some(index) = selected else {
                    ui.label("Select an item to edit it.");
                    ui.label("Hold Space and drag to pan. Use the wheel to zoom.");
                    return;
                };
                ui.heading("Selected item");
                let element = &mut self.scene.elements[index];
                ui.label(element.media_type.to_uppercase());
                ui.label(egui::RichText::new(&element.path).small().weak());
                ui.horizontal(|ui| {
                    ui.label("X");
                    ui.add(egui::DragValue::new(&mut element.x).speed(1.0));
                    ui.label("Y");
                    ui.add(egui::DragValue::new(&mut element.y).speed(1.0));
                });
                let mut width = element.width.unwrap_or(320.0);
                let mut height = element.height.unwrap_or(200.0);
                ui.horizontal(|ui| {
                    ui.label("W");
                    ui.add(egui::DragValue::new(&mut width).range(10.0..=10000.0));
                });
                ui.horizontal(|ui| {
                    ui.label("H");
                    ui.add(egui::DragValue::new(&mut height).range(10.0..=10000.0));
                });
                element.width = Some(width);
                element.height = Some(height);
                if element.is_video() {
                    let duration = if element.media_type == "youtube" {
                        element.source_range.map(|r| r[1] - r[0])
                    } else {
                        element.media_duration
                    };
                    if let Some(duration) = duration {
                        ui.label(format!("Duration {:.2}s", duration));
                    }
                    let mut pairs = if element.media_type == "youtube"
                        && !element.playback_loop_pairs.is_empty()
                    {
                        element.playback_loop_pairs.clone()
                    } else {
                        element.loop_pairs.clone()
                    };
                    let active_pair = element.active_playback_pair();
                    let active_pair = active_pair.min(pairs.len().saturating_sub(1));
                    if let Some(pair) = pairs.get_mut(active_pair) {
                        ui.label("Loop range (%)");
                        let before = *pair;
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut pair[0])
                                    .range(0.0..=99.0)
                                    .speed(0.5),
                            );
                            ui.add(
                                egui::DragValue::new(&mut pair[1])
                                    .range(1.0..=100.0)
                                    .speed(0.5),
                            );
                        });
                        if pair[0] >= pair[1] {
                            pair[0] = (pair[1] - 1.0).max(0.0);
                        }
                        if *pair != before {
                            restart_video = Some(element.id);
                        }
                        if element.media_type == "youtube" {
                            element.playback_loop_pairs = pairs;
                        } else {
                            element.loop_pairs = pairs;
                        }
                    }
                    if element.media_type == "youtube" {
                        if element.cached_path.as_ref().is_some_and(|p| p.exists()) {
                            if ui.button("Download another section…").clicked() {
                                youtube_action = Some((element.path.clone(), element.id));
                            }
                        } else if ui.button("Choose section and download…").clicked() {
                            youtube_action = Some((element.path.clone(), element.id));
                        }
                    }
                    if ui
                        .button(if self.edit_video_id == Some(element.id) {
                            "Exit video edit"
                        } else {
                            "Edit video"
                        })
                        .clicked()
                    {
                        edit_action = Some(element.id);
                    }
                }
                delete_selected = ui.button("Delete item").clicked();
            });
        if let Some((url, id)) = youtube_action {
            self.start_youtube_metadata(url, Some(id));
        }
        if let Some(id) = restart_video {
            self.video_players.remove(&id.to_string());
            self.video_textures.remove(&id.to_string());
        }
        if let Some(id) = edit_action {
            if self.edit_video_id == Some(id) {
                self.exit_video_edit();
            } else {
                self.enter_video_edit(id);
            }
        }
        if delete_selected && let Some(index) = selected {
            let removed = self.scene.elements.remove(index);
            self.clear_item_cache(removed.id);
            self.selected = None;
        }
    }

    fn draw_canvas(&mut self, ctx: &EguiContext) {
        self.ensure_assets(ctx);
        let modal_active = self.show_youtube.is_some() || self.show_text_dialog || self.show_help;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(30, 30, 32)))
            .show(ctx, |ui| {
                let canvas_rect = ui.max_rect();
                let canvas = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
                if !modal_active {
                    canvas.context_menu(|ui| {
                        if ui.button("Paste").clicked() {
                            self.handle_paste();
                            ui.close();
                        }
                        if ui.button("Load…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("AnimRef scene", &["purgif"])
                                .pick_file()
                            {
                                let result = self.load_scene(&path);
                                self.capture_result(result);
                            }
                            ui.close();
                        }
                        if ui.button("Save").clicked() {
                            let result = self.save_scene();
                            self.capture_result(result);
                            ui.close();
                        }
                        if ui.button("Save As…").clicked() {
                            let result = self.save_scene_as();
                            self.capture_result(result);
                            ui.close();
                        }
                        if ui.button("New").clicked() {
                            self.new_scene();
                            ui.close();
                        }
                        ui.menu_button("Recent", |ui| {
                            for path in self.recent.clone().into_iter().rev() {
                                if ui.button(path.to_string_lossy()).clicked() {
                                    let result = self.load_scene(&path);
                                    self.capture_result(result);
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("Window", |ui| {
                            let mut always_on_top = self.always_on_top;
                            if ui.checkbox(&mut always_on_top, "Always on top").changed() {
                                self.always_on_top = always_on_top;
                                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
                                    if always_on_top {
                                        WindowLevel::AlwaysOnTop
                                    } else {
                                        WindowLevel::Normal
                                    },
                                ));
                            }
                            if ui.button("Toggle maximize").clicked() {
                                let maximized =
                                    ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                                ui.close();
                            }
                            if ui.button("Minimize").clicked() {
                                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                                ui.close();
                            }
                            if ui.button("Close").clicked() {
                                ctx.send_viewport_cmd(ViewportCommand::Close);
                            }
                        });
                        if let Some(index) =
                            self.selected.filter(|i| *i < self.scene.elements.len())
                            && self.scene.elements[index].is_video()
                            && ui
                                .button(if self.edit_video_id.is_some() {
                                    "Close edit video"
                                } else {
                                    "Edit video"
                                })
                                .clicked()
                        {
                            if self.edit_video_id.is_some() {
                                self.exit_video_edit();
                            } else {
                                let id = self.scene.elements[index].id;
                                self.enter_video_edit(id);
                            }
                            ui.close();
                        }
                    });
                }
                let painter = ui.painter_at(canvas_rect);
                let (space_pan, ctrl_space_zoom, right_drag) = ctx.input(|i| {
                    (
                        i.pointer.primary_down() && i.key_down(Key::Space) && !i.modifiers.ctrl,
                        i.pointer.primary_down() && i.key_down(Key::Space) && i.modifiers.ctrl,
                        i.pointer.button_down(egui::PointerButton::Secondary),
                    )
                });
                if self.scene.elements.is_empty() {
                    painter.text(
                        canvas_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Drop images, GIFs, videos, or paste a YouTube URL",
                        FontId::proportional(18.0),
                        Color32::from_gray(130),
                    );
                }
                let pointer = ctx.input(|i| i.pointer.interact_pos());
                let mut bring_front_id = None;
                for index in 0..self.scene.elements.len() {
                    let editing_this = self.edit_video_id == Some(self.scene.elements[index].id);
                    if self.edit_video_id.is_some() && !editing_this {
                        continue;
                    }
                    let (world_x, world_y, width, height) = {
                        let element = &self.scene.elements[index];
                        (
                            element.x,
                            element.y,
                            element.width.unwrap_or(320.0),
                            element.height.unwrap_or(200.0),
                        )
                    };
                    let scale = self.scene.current_scale;
                    let origin =
                        canvas_rect.min + Vec2::new(self.scene.translate.x, self.scene.translate.y);
                    let min = if editing_this {
                        canvas_rect.min + Vec2::splat(8.0)
                    } else {
                        origin + Vec2::new(world_x, world_y) * scale
                    };
                    let rect = if editing_this {
                        canvas_rect.shrink(8.0)
                    } else {
                        Rect::from_min_size(min, Vec2::new(width, height) * scale)
                    };
                    let response = ui.interact(rect, ui.id().with(index), Sense::click_and_drag());
                    let selected_item = self.selected == Some(index) && !editing_this;
                    let handle_rect = Rect::from_min_size(
                        rect.right_bottom() - Vec2::splat(12.0),
                        Vec2::splat(12.0),
                    );
                    let handle = if !modal_active && selected_item {
                        Some(ui.interact(
                            handle_rect,
                            ui.id().with(("resize", index)),
                            Sense::drag(),
                        ))
                    } else {
                        None
                    };
                    if !modal_active && response.clicked() {
                        self.selected = Some(index);
                        bring_front_id = Some(self.scene.elements[index].id);
                    }
                    if !modal_active
                        && response.dragged()
                        && !handle.as_ref().is_some_and(|r| r.dragged())
                        && !editing_this
                        && !space_pan
                        && !ctrl_space_zoom
                    {
                        let delta = ctx.input(|i| i.pointer.delta()) / scale;
                        self.scene.elements[index].x += delta.x;
                        self.scene.elements[index].y += delta.y;
                        self.selected = Some(index);
                        ctx.request_repaint();
                    }
                    if !modal_active
                        && selected_item
                        && handle.as_ref().is_some_and(|r| r.dragged())
                    {
                        let delta = ctx.input(|i| i.pointer.delta()) / scale;
                        let element = &mut self.scene.elements[index];
                        let aspect = (width / height).max(0.01);
                        if (delta.x / width.max(1.0)).abs() >= (delta.y / height.max(1.0)).abs() {
                            let new_width = (width + delta.x).max(10.0);
                            element.width = Some(new_width);
                            element.height = Some((new_width / aspect).max(10.0));
                        } else {
                            let new_height = (height + delta.y).max(10.0);
                            element.height = Some(new_height);
                            element.width = Some((new_height * aspect).max(10.0));
                        }
                        ctx.request_repaint();
                    }
                    self.paint_element(&painter, index, rect, ctx);
                    if self.selected == Some(index) && !editing_this {
                        let handle_rect = Rect::from_min_size(
                            rect.right_bottom() - Vec2::splat(12.0),
                            Vec2::splat(12.0),
                        );
                        painter.rect_stroke(
                            rect,
                            0.0,
                            Stroke::new(1.5_f32, Color32::from_rgb(45, 165, 210)),
                            egui::StrokeKind::Inside,
                        );
                        painter.rect_filled(handle_rect, 1.0, Color32::from_rgb(45, 165, 210));
                    }
                    if self.scene.elements[index].is_video() {
                        if let Some(error) = self.video_errors.get(&self.video_key(index)) {
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                error,
                                FontId::proportional(13.0),
                                Color32::LIGHT_RED,
                            );
                        } else if self.scene.elements[index].media_type == "youtube"
                            && self.scene.elements[index].display_path().is_none()
                        {
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Select a source section and download",
                                FontId::proportional(15.0),
                                Color32::LIGHT_GRAY,
                            );
                        }
                    }
                }
                if let Some(id) = bring_front_id
                    && let Some(index) = self
                        .scene
                        .elements
                        .iter()
                        .position(|element| element.id == id)
                {
                    self.bring_to_front(index);
                }
                if !modal_active
                    && (canvas_rect.contains(pointer.unwrap_or_default())
                        && (space_pan
                            || ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle))))
                {
                    let delta = ctx.input(|i| i.pointer.delta());
                    self.scene.translate.x += delta.x;
                    self.scene.translate.y += delta.y;
                }
                if !modal_active && right_drag && pointer.is_some_and(|p| canvas_rect.contains(p)) {
                    let delta = ctx.input(|i| i.pointer.delta());
                    if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(outer.min + delta));
                    }
                }
                if let Some(pos) = pointer {
                    let scroll = ctx.input(|i| i.raw_scroll_delta.y);
                    if !modal_active
                        && self.edit_video_id.is_none()
                        && scroll != 0.0
                        && canvas_rect.contains(pos)
                    {
                        let old_scale = self.scene.current_scale;
                        let new_scale = (old_scale * (1.0 + scroll * 0.001)).clamp(0.01, 2.0);
                        let anchor = pos
                            - canvas_rect.min
                            - Vec2::new(self.scene.translate.x, self.scene.translate.y);
                        let factor = 1.0 - new_scale / old_scale;
                        self.scene.translate.x += anchor.x * factor;
                        self.scene.translate.y += anchor.y * factor;
                        self.scene.current_scale = new_scale;
                        ctx.request_repaint();
                    }
                    if !modal_active
                        && self.edit_video_id.is_none()
                        && ctrl_space_zoom
                        && canvas_rect.contains(pos)
                    {
                        let delta = ctx.input(|i| i.pointer.delta().x);
                        if delta != 0.0 {
                            let old_scale = self.scene.current_scale;
                            let new_scale =
                                (old_scale + delta / 120.0 * old_scale / 2.0).clamp(0.01, 2.0);
                            let anchor = pos
                                - canvas_rect.min
                                - Vec2::new(self.scene.translate.x, self.scene.translate.y);
                            let factor = 1.0 - new_scale / old_scale;
                            self.scene.translate.x += anchor.x * factor;
                            self.scene.translate.y += anchor.y * factor;
                            self.scene.current_scale = new_scale;
                        }
                    }
                }
                if !modal_active && ctx.input(|i| !i.raw.dropped_files.is_empty()) {
                    for file in ctx.input(|i| i.raw.dropped_files.clone()) {
                        if let Some(path) = file.path {
                            self.import_path(&path);
                        }
                    }
                }
                let _ = canvas; // reserve canvas input surface for panning, drops, and viewport drag
            });
    }

    fn paint_element(
        &mut self,
        painter: &egui::Painter,
        index: usize,
        rect: Rect,
        ctx: &EguiContext,
    ) {
        let element = &self.scene.elements[index];
        if element.media_type == "text" {
            let desired = (rect.height() / 2.2).clamp(0.01, 72.0);
            let mut low = 0.01_f32;
            let mut high = desired;
            let mut fitted = painter.layout(
                element.path.clone(),
                FontId::proportional(low),
                Color32::WHITE,
                rect.width().max(0.01),
            );
            for _ in 0..18 {
                let candidate = (low + high) * 0.5;
                let galley = painter.layout(
                    element.path.clone(),
                    FontId::proportional(candidate),
                    Color32::WHITE,
                    rect.width().max(0.01),
                );
                if galley.size().x <= rect.width() && galley.size().y <= rect.height() {
                    low = candidate;
                    fitted = galley;
                } else {
                    high = candidate;
                }
            }
            let pos = rect.center() - fitted.size() * 0.5;
            painter
                .with_clip_rect(rect)
                .galley(pos, fitted, Color32::WHITE);
            return;
        }
        if element.is_video() {
            let key = self.video_key(index);
            if let Some(texture) = self.video_textures.get(&key) {
                let video_rect = contain_rect(rect, texture.size_vec2());
                painter.image(
                    texture.id(),
                    video_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                painter.rect_filled(rect, 0.0, Color32::from_gray(18));
            }
            return;
        }
        let key = element.id.to_string();
        if let Some(gif) = self.gif_assets.get(&key) {
            let elapsed = gif.started.elapsed().as_secs_f32();
            let total = gif.total_duration.as_secs_f32().max(0.02);
            let mut tick = elapsed % total;
            let mut frame = 0;
            for (index, (_, delay)) in gif.frames.iter().enumerate() {
                if tick < delay.as_secs_f32() {
                    frame = index;
                    break;
                }
                tick -= delay.as_secs_f32();
            }
            let texture = &gif.frames[frame].0;
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            ctx.request_repaint_after(Duration::from_millis(25));
        } else if let Some(texture) = self.image_assets.get(&key) {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else if let Some(error) = self.image_errors.get(&key) {
            painter.rect_filled(rect, 2.0, Color32::from_rgb(80, 35, 35));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                error,
                FontId::proportional(13.0),
                Color32::WHITE,
            );
        } else {
            painter.rect_filled(rect, 2.0, Color32::from_gray(55));
        }
    }

    fn video_key(&self, index: usize) -> String {
        self.scene.elements[index].id.to_string()
    }

    fn bring_to_front(&mut self, index: usize) {
        if index >= self.scene.elements.len() {
            return;
        }
        let element = self.scene.elements.remove(index);
        self.scene.elements.push(element);
        self.selected = Some(self.scene.elements.len() - 1);
    }

    fn draw_youtube_dialog(&mut self, ctx: &EguiContext) {
        let mut finished_download = None;
        {
            let Some(dialog) = self.show_youtube.as_mut() else {
                return;
            };
            let metadata_events = dialog.metadata_job.as_ref().map(|job| job.events.clone());
            let mut metadata_finished = false;
            if let Some(rx) = metadata_events {
                match rx.try_recv() {
                    Ok(Ok(metadata)) => {
                        if let Some(id) = dialog.target_element
                            && let Some(element) =
                                self.scene.elements.iter_mut().find(|e| e.id == id)
                        {
                            element.source_duration = metadata.duration;
                            if let Some([start, end]) = element.source_range {
                                dialog.start_text = start.to_string();
                                dialog.end_text = end.to_string();
                            } else if let Some(pair) =
                                element.loop_pairs.get(element.active_loop_pair)
                            {
                                let duration = metadata.duration.unwrap_or(0.0);
                                dialog.start_text = (duration * pair[0] / 100.0).to_string();
                                dialog.end_text = (duration * pair[1] / 100.0).to_string();
                            }
                        }
                        if dialog.end_text.trim().is_empty() {
                            dialog.end_text = metadata.duration.unwrap_or(0.0).to_string();
                        }
                        dialog.metadata = Some(metadata);
                        dialog.metadata_waiting = false;
                        metadata_finished = true;
                    }
                    Ok(Err(error)) => {
                        dialog.error = Some(error);
                        dialog.metadata_waiting = false;
                        metadata_finished = true;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        dialog.error = Some("Metadata lookup stopped unexpectedly.".into());
                        dialog.metadata_waiting = false;
                        metadata_finished = true;
                    }
                }
            }
            if metadata_finished {
                dialog.metadata_job = None;
            }

            let event_rx = dialog.job.as_ref().map(|job| job.events.clone());
            let mut completed_path = None;
            let mut job_finished = false;
            if let Some(event_rx) = event_rx {
                while let Ok(event) = event_rx.try_recv() {
                    match event {
                        DownloadEvent::Progress(progress) => dialog.progress = progress,
                        DownloadEvent::Finished(path) => {
                            completed_path = Some(path);
                            job_finished = true;
                        }
                        DownloadEvent::Failed(error) => {
                            dialog.error = Some(error);
                            job_finished = true;
                        }
                        DownloadEvent::Cancelled => {
                            dialog.error = Some("Download cancelled".into());
                            job_finished = true;
                        }
                    }
                }
            }
            if job_finished {
                dialog.job = None;
            }
            if let (Some(path), Some(metadata)) = (completed_path, dialog.metadata.clone()) {
                finished_download = Some(CompletedYoutubeDownload {
                    path,
                    metadata,
                    url: dialog.url.clone(),
                    target_element: dialog.target_element,
                    generation: dialog.generation,
                    start_text: dialog.start_text.clone(),
                    end_text: dialog.end_text.clone(),
                });
            }
        }
        if let Some(completed) = finished_download {
            self.finish_youtube_download(completed);
            self.show_youtube = None;
            return;
        }
        let Some(dialog) = self.show_youtube.as_mut() else {
            return;
        };
        let mut close = false;
        egui::Window::new("Prepare YouTube clip")
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(ctx, |ui| {
                ui.label("Paste a YouTube link, inspect its metadata, then choose the source range to download.");
                let mut url_changed = false;
                ui.add_enabled_ui(dialog.job.is_none() && !dialog.metadata_waiting, |ui| {
                    url_changed = ui.text_edit_singleline(&mut dialog.url).changed();
                });
                if url_changed {
                    dialog.metadata = None;
                    dialog.start_text = "0".into();
                    dialog.end_text.clear();
                    dialog.error = None;
                }
                if ui.add_enabled(!dialog.metadata_waiting && dialog.job.is_none(), egui::Button::new(if dialog.metadata_waiting { "Checking…" } else { "Inspect video" })).clicked() {
                    let url = dialog.url.clone();
                    match youtube::begin_metadata(url) {
                        Ok(job) => {
                            dialog.metadata_job = Some(job);
                            dialog.metadata_waiting = true;
                            dialog.error = None;
                        }
                        Err(error) => dialog.error = Some(error.to_string()),
                    }
                }
                if let Some(metadata) = &dialog.metadata {
                    ui.separator();
                    ui.label(egui::RichText::new(&metadata.title).strong());
                    ui.label(format!("Source duration: {:.2}s", metadata.duration.unwrap_or(0.0)));
                    ui.add_enabled_ui(dialog.job.is_none(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Start"); ui.text_edit_singleline(&mut dialog.start_text);
                            ui.label("End"); ui.text_edit_singleline(&mut dialog.end_text);
                        });
                    });
                    ui.label("Times can be seconds or HH:MM:SS. The source video is not downloaded until you click Download section.");
                    if let Some(job) = &dialog.job {
                        ui.add(egui::ProgressBar::new(dialog.progress / 100.0).show_percentage());
                        if ui.button("Cancel download").clicked() { job.cancel(); }
                    } else if ui.button("Download selected section").clicked() {
                        let start = parse_time(&dialog.start_text);
                        let end = parse_time(&dialog.end_text);
                        let duration = metadata.duration.unwrap_or(0.0);
                        if let (Some(start), Some(end)) = (start, end) {
                            if start >= 0.0 && start < end && end <= duration + 0.01 {
                                match youtube::begin_download(dialog.url.clone(), metadata.id.clone(), start, end, "best".into()) {
                                    Ok(job) => { dialog.job = Some(job); dialog.progress = 0.0; dialog.error = None; }
                                    Err(error) => dialog.error = Some(error.to_string()),
                                }
                            } else { dialog.error = Some("Choose a valid range within the source duration.".into()); }
                        } else { dialog.error = Some("Enter the times as seconds or HH:MM:SS.".into()); }
                    }
                }
                if let Some(error) = &dialog.error { ui.colored_label(Color32::LIGHT_RED, error); }
                ui.separator();
                if ui.button("Close").clicked() { close = true; }
            });
        if close {
            self.show_youtube = None;
        }
        ctx.request_repaint_after(Duration::from_millis(75));
    }

    fn finish_youtube_download(&mut self, completed: CompletedYoutubeDownload) {
        let CompletedYoutubeDownload {
            path,
            metadata,
            url,
            target_element,
            generation,
            start_text,
            end_text,
        } = completed;
        if generation != self.scene_generation {
            return;
        }
        let Some(start) = parse_time(&start_text) else {
            self.status = "Invalid start time".into();
            return;
        };
        let Some(end) = parse_time(&end_text) else {
            self.status = "Invalid end time".into();
            return;
        };
        let clip_duration = media::probe_duration(&path).ok().unwrap_or(end - start);
        let target = target_element.and_then(|id| {
            self.scene
                .elements
                .iter()
                .position(|element| element.id == id)
        });
        if target_element.is_some() && target.is_none() {
            self.status = "The original YouTube board item no longer exists.".into();
            return;
        }
        if let Some(index) = target {
            let key = self.scene.elements[index].id.to_string();
            self.video_players.remove(&key);
            self.video_textures.remove(&key);
            let element = &mut self.scene.elements[index];
            element.path = url.clone();
            element.media_type = "youtube".into();
            element.source_duration = metadata.duration;
            element.source_range = Some([start, end]);
            element.cached_path = Some(path.clone());
            element.media_duration = Some(clip_duration);
            element.playback_loop_pairs = vec![[0.0, 100.0]];
            element.active_playback_loop_pair = Some(0);
        } else {
            let mut element = Element::new(url, "youtube", 100.0, 100.0, 640.0, 360.0);
            element.source_duration = metadata.duration;
            element.source_range = Some([start, end]);
            element.cached_path = Some(path.clone());
            element.playback_loop_pairs = vec![[0.0, 100.0]];
            element.active_playback_loop_pair = Some(0);
            element.media_duration = Some(clip_duration);
            self.scene.elements.push(element);
            self.selected = self.scene.elements.len().checked_sub(1);
        }
        self.status = format!(
            "Downloaded {:.2} seconds from {}",
            end - start,
            metadata.title
        );
    }

    fn draw_other_windows(&mut self, ctx: &EguiContext) {
        if self.show_text_dialog {
            egui::Window::new("Add text")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.text_edit_multiline(&mut self.text_draft);
                    if ui.button("Add").clicked() {
                        let draft = std::mem::take(&mut self.text_draft);
                        self.add_text(draft);
                        self.show_text_dialog = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_text_dialog = false;
                    }
                });
        }
        if self.show_help {
            egui::Window::new("AnimRef controls").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label("Drag items to move them. Drag the blue lower corner to resize.");
                ui.label("Hold Space and drag, or use the middle mouse button, to pan.");
                ui.label("Use the wheel to zoom around the pointer.");
        ui.label("Ctrl+L opens, Ctrl+S saves, Ctrl+N starts a scene, Delete removes an item.");
                ui.label("Paste a YouTube URL to inspect it and choose a section before download.");
                if ui.button("Close").clicked() { self.show_help = false; }
            });
        }
    }

    fn begin_window_resize(&self, ctx: &EguiContext) {
        if self.show_youtube.is_some() || self.show_text_dialog || self.show_help {
            return;
        }
        let (rect, pointer, pressed) = ctx.input(|i| {
            (
                i.viewport().inner_rect,
                i.pointer.interact_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
            )
        });
        let (Some(window_rect), Some(pointer)) = (rect, pointer) else {
            return;
        };
        if !pressed {
            return;
        }
        let rect = Rect::from_min_size(Pos2::ZERO, window_rect.size());
        let edge = 6.0;
        let left = pointer.x <= rect.left() + edge;
        let right = pointer.x >= rect.right() - edge;
        let top = pointer.y <= rect.top() + edge;
        let bottom = pointer.y >= rect.bottom() - edge;
        let direction = match (top, bottom, left, right) {
            (true, _, true, _) => Some(egui::viewport::ResizeDirection::NorthWest),
            (true, _, _, true) => Some(egui::viewport::ResizeDirection::NorthEast),
            (_, true, true, _) => Some(egui::viewport::ResizeDirection::SouthWest),
            (_, true, _, true) => Some(egui::viewport::ResizeDirection::SouthEast),
            (true, _, _, _) => Some(egui::viewport::ResizeDirection::North),
            (_, true, _, _) => Some(egui::viewport::ResizeDirection::South),
            (_, _, true, _) => Some(egui::viewport::ResizeDirection::West),
            (_, _, _, true) => Some(egui::viewport::ResizeDirection::East),
            _ => None,
        };
        if let Some(direction) = direction {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }

    fn handle_shortcuts(&mut self, ctx: &EguiContext) {
        let wants_keyboard = ctx.wants_keyboard_input();
        let save_as = !wants_keyboard && consume_ctrl_shortcut(ctx, Key::S, true);
        if save_as {
            let result = self.save_scene_as();
            self.capture_result(result);
        }
        if !wants_keyboard
            && consume_ctrl_shortcut(ctx, Key::L, false)
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("AnimRef scene", &["purgif"])
                .pick_file()
        {
            let result = self.load_scene(&path);
            self.capture_result(result);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::W, false) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::F, false) {
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::M, false) {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::H, false) {
            self.show_help = true;
        }
        if !save_as && !wants_keyboard && consume_ctrl_shortcut(ctx, Key::S, false) {
            let result = self.save_scene();
            self.capture_result(result);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::N, false) {
            self.new_scene();
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::V, false) {
            self.handle_paste();
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) && self.edit_video_id.is_some() {
            self.exit_video_edit();
        }
        if ctx.input(|i| i.key_pressed(Key::Delete))
            && !wants_keyboard
            && let Some(index) = self
                .selected
                .take()
                .filter(|i| *i < self.scene.elements.len())
        {
            let removed = self.scene.elements.remove(index);
            self.clear_item_cache(removed.id);
        }
    }

    fn capture_result(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.status = format!("{error:#}");
        }
    }

    fn draw_edit_timeline(&mut self, ctx: &EguiContext) {
        let Some(id) = self.edit_video_id else {
            return;
        };
        let Some(index) = self
            .scene
            .elements
            .iter()
            .position(|element| element.id == id)
        else {
            self.exit_video_edit();
            return;
        };
        let element = &self.scene.elements[index];
        let is_youtube = element.media_type == "youtube";
        let active = if is_youtube {
            element.active_playback_pair()
        } else {
            element.active_loop_pair
        };
        let duration = element.media_duration.unwrap_or(0.0);
        let element_id = element.id;
        let player_key = element_id.to_string();
        let players = &self.video_players;
        let textures = &mut self.video_textures;
        let pending_seek = &mut self.pending_video_seek;
        let mut done = false;
        egui::TopBottomPanel::bottom("edit-timeline")
            .exact_height(58.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(48, 44, 51)))
            .show(ctx, |ui| {
                let element = &mut self.scene.elements[index];
                let pairs =
                    if element.media_type == "youtube" && !element.playback_loop_pairs.is_empty() {
                        &mut element.playback_loop_pairs
                    } else {
                        &mut element.loop_pairs
                    };
                if pairs.is_empty() {
                    pairs.push([0.0, 100.0]);
                }
                let active = active.min(pairs.len().saturating_sub(1));
                ui.horizontal(|ui| {
                    ui.strong("Loop range");
                    let mut playhead = players
                        .get(&player_key)
                        .map(|player| player.playhead())
                        .unwrap_or(0.0);
                    if duration > 0.0 {
                        let position_before = playhead;
                        ui.add(
                            egui::Slider::new(&mut playhead, 0.0..=duration)
                                .text("Position")
                                .show_value(true),
                        );
                        if (playhead - position_before).abs() > 0.001 {
                            pending_seek.insert(element_id, playhead);
                            // Dropping the current player stops its FFmpeg process.
                            // Remove it after this panel has finished borrowing the map.
                        }
                    }
                    let pair = &mut pairs[active];
                    let before = *pair;
                    ui.add(
                        egui::Slider::new(&mut pair[0], 0.0..=99.0)
                            .text("A")
                            .show_value(true),
                    );
                    ui.add(
                        egui::Slider::new(&mut pair[1], 1.0..=100.0)
                            .text("B")
                            .show_value(true),
                    );
                    if pair[0] >= pair[1] {
                        pair[0] = (pair[1] - 1.0).max(0.0);
                    }
                    if *pair != before && duration > 0.0 {
                        let seek =
                            (duration * pair[0] / 100.0).clamp(0.0, (duration - 0.01).max(0.0));
                        pending_seek.insert(element_id, seek);
                    }
                    if ui.button("Done").clicked() {
                        done = true;
                    }
                });
            });
        if pending_seek.contains_key(&element_id) {
            self.video_players.remove(&player_key);
            textures.remove(&player_key);
        }
        if done {
            self.exit_video_edit();
        }
    }
}

impl eframe::App for AnimRefApp {
    fn update(&mut self, ctx: &EguiContext, _frame: &mut eframe::Frame) {
        self.update_diagnostics(ctx);
        self.handle_shortcuts(ctx);
        self.drain_remote_imports();
        self.draw_top_bar(ctx);
        self.draw_inspector(ctx);
        self.draw_edit_timeline(ctx);
        egui::TopBottomPanel::bottom("status")
            .exact_height(22.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&self.status).small().weak());
            });
        self.draw_canvas(ctx);
        self.draw_youtube_dialog(ctx);
        self.draw_other_windows(ctx);
        self.begin_window_resize(ctx);
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn consume_ctrl_shortcut(ctx: &EguiContext, key: Key, shift: bool) -> bool {
    let modifiers = if shift {
        egui::Modifiers::CTRL | egui::Modifiers::SHIFT
    } else {
        egui::Modifiers::CTRL
    };
    ctx.input_mut(|input| input.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, key)))
}

fn read_image_source(path: &str) -> Result<Vec<u8>> {
    if let Some(rest) = path.strip_prefix("data:") {
        let (metadata, payload) = rest.split_once(',').context("Invalid image data URL")?;
        if metadata.contains(";base64") {
            return Ok(base64::engine::general_purpose::STANDARD.decode(payload)?);
        }
        return Ok(payload.as_bytes().to_vec());
    }
    fs::read(path).with_context(|| format!("Could not read image {}", path))
}

fn resolve_scene_asset_paths(scene: &mut Scene, parent: &Path) {
    for element in &mut scene.elements {
        if matches!(element.media_type.as_str(), "img" | "video" | "filePath")
            && !element.path.starts_with("data:")
            && !element.path.starts_with("http://")
            && !element.path.starts_with("https://")
        {
            let asset = PathBuf::from(&element.path);
            if asset.is_relative() {
                element.path = parent.join(asset).to_string_lossy().into_owned();
            }
        }
        if let Some(cached) = element.cached_path.as_mut()
            && cached.is_relative()
        {
            *cached = parent.join(&*cached);
        }
    }
}

fn fill_missing_dimensions(element: &mut Element, source_width: f32, source_height: f32) {
    if source_width <= 0.0 || source_height <= 0.0 {
        return;
    }
    match (element.width, element.height) {
        (None, None) => {
            element.width = Some(source_width);
            element.height = Some(source_height);
        }
        (Some(width), None) if width > 0.0 => {
            element.height = Some(width * source_height / source_width);
        }
        (None, Some(height)) if height > 0.0 => {
            element.width = Some(height * source_width / source_height);
        }
        _ => (),
    }
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn remote_url_is_video(value: &str) -> bool {
    let extension = value
        .split(['?', '#'])
        .next()
        .unwrap_or(value)
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi"
    )
}

fn contain_rect(bounds: Rect, source_size: Vec2) -> Rect {
    if source_size.x <= 0.0 || source_size.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width() / source_size.x).min(bounds.height() / source_size.y);
    let size = source_size * scale;
    Rect::from_center_size(bounds.center(), size)
}

fn is_gif_source(path: &str, bytes: &[u8]) -> bool {
    path.to_ascii_lowercase().ends_with(".gif")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
}

fn clipboard_path(text: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(text.trim().trim_matches('"'));
    (candidate.is_file()).then_some(candidate)
}

fn install_cjk_font(ctx: &EguiContext) {
    let mut fonts = egui::FontDefinitions::default();
    for candidate in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ] {
        if let Ok(bytes) = fs::read(candidate) {
            fonts.font_data.insert(
                "animref-cjk".into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(entries) = fonts.families.get_mut(&family) {
                    entries.push("animref-cjk".into());
                }
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

fn is_youtube_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("youtube.com/") || lower.contains("youtu.be/"))
        && (lower.starts_with("http://") || lower.starts_with("https://"))
}

fn parse_time(text: &str) -> Option<f64> {
    let parts: Vec<_> = text.trim().split(':').collect();
    let seconds = match parts.as_slice() {
        [s] => s.parse::<f64>().ok()?,
        [m, s] => m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?,
        [h, m, s] => {
            h.parse::<f64>().ok()? * 3600.0
                + m.parse::<f64>().ok()? * 60.0
                + s.parse::<f64>().ok()?
        }
        _ => return None,
    };
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

fn summarize_input_event(event: &egui::Event) -> String {
    match event {
        egui::Event::Screenshot { image, .. } => {
            format!("Screenshot({}x{})", image.size[0], image.size[1])
        }
        egui::Event::Text(text) => {
            format!("Text({:?})", text.chars().take(128).collect::<String>())
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
fn recent_file() -> PathBuf {
    static TEST_RECENT_DIRECTORY: std::sync::OnceLock<tempfile::TempDir> =
        std::sync::OnceLock::new();
    TEST_RECENT_DIRECTORY
        .get_or_init(|| tempfile::tempdir().expect("create isolated test recent directory"))
        .path()
        .join("recent.json")
}

#[cfg(not(test))]
fn recent_file() -> PathBuf {
    directories::ProjectDirs::from("", "AnimRef", "AnimRef")
        .map(|p| p.data_dir().join("recent.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("AnimRef-recent.json"))
}

fn load_recent() -> Vec<PathBuf> {
    let path = recent_file();
    let saved = if path.is_file() {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<PathBuf>>(&text).ok())
            .unwrap_or_default()
    } else {
        legacy_recent_paths()
    };
    saved.into_iter().filter(|p| p.exists()).collect()
}

fn legacy_recent_paths() -> Vec<PathBuf> {
    let Some(app_data) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let config = PathBuf::from(app_data).join("animref").join("config.json");
    fs::read_to_string(config)
        .ok()
        .and_then(|text| {
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            let recent = value.get("recent")?.as_str()?;
            serde_json::from_str::<Vec<PathBuf>>(recent).ok()
        })
        .unwrap_or_default()
}

fn store_recent(paths: &[PathBuf]) {
    let path = recent_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(paths) {
        let _ = fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_for_shortcut_test(scene_path: PathBuf) -> AnimRefApp {
        AnimRefApp {
            scene: Scene::default(),
            selected: None,
            scene_path: Some(scene_path),
            recent: Vec::new(),
            status: String::new(),
            always_on_top: true,
            show_help: false,
            show_youtube: None,
            show_text_dialog: false,
            text_draft: String::new(),
            image_assets: HashMap::new(),
            gif_assets: HashMap::new(),
            image_errors: HashMap::new(),
            video_players: HashMap::new(),
            video_textures: HashMap::new(),
            video_errors: HashMap::new(),
            pending_video_seek: HashMap::new(),
            remote_jobs: Vec::new(),
            remote_pending: HashSet::new(),
            scene_generation: 1,
            edit_video_id: None,
            edit_view_backup: None,
            cli: CliOptions::default(),
            started_at: Instant::now(),
            update_count: 0,
            decoded_video_frames: 0,
            decoded_video_frames_by_id: HashMap::new(),
            screenshot_requested: false,
            screenshot_result: None,
            smoke_report_written: false,
            smoke_report_update: None,
            diagnostic_input_events: VecDeque::with_capacity(32),
            diagnostic_modifiers: String::new(),
            diagnostic_wants_keyboard: false,
        }
    }

    fn canvas_frame(context: &EguiContext, app: &mut AnimRefApp, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = context.run(input, |ctx| app.draw_canvas(ctx));
    }

    #[test]
    fn parses_supported_time_forms() {
        assert_eq!(parse_time("93.5"), Some(93.5));
        assert_eq!(parse_time("01:33.5"), Some(93.5));
        assert_eq!(parse_time("00:01:33.500"), Some(93.5));
        assert_eq!(parse_time("1:2:3:4"), None);
    }

    #[test]
    fn parses_cli_diagnostic_paths() {
        let options = CliOptions::parse(
            [
                "--scene",
                "target/acceptance/legacy.purgif",
                "--screenshot",
                "target/acceptance/render.png",
                "--smoke-report",
                "target/acceptance/report.json",
                "--quit-after-smoke",
            ]
            .map(std::ffi::OsString::from),
        );
        assert_eq!(
            options.scene_path,
            Some(PathBuf::from("target/acceptance/legacy.purgif"))
        );
        assert_eq!(
            options.screenshot_path,
            Some(PathBuf::from("target/acceptance/render.png"))
        );
        assert_eq!(
            options.smoke_report_path,
            Some(PathBuf::from("target/acceptance/report.json"))
        );
        assert!(options.quit_after_smoke);
    }

    #[test]
    fn resolving_and_round_tripping_relative_scene_assets_is_idempotent() {
        let directory = tempfile::tempdir().expect("create project directory");
        let scene_parent = directory.path().join("scenes");
        fs::create_dir_all(&scene_parent).expect("create scenes directory");
        let mut scene = Scene::default();
        let mut image = Element::new(
            "../media/missing.png".to_owned(),
            "img",
            5.0,
            7.0,
            200.0,
            100.0,
        );
        image.cached_path = Some(PathBuf::from("../cache/cached.png"));
        scene.elements.push(image);

        resolve_scene_asset_paths(&mut scene, &scene_parent);
        let first_path = scene.elements[0].path.clone();
        let first_cache = scene.elements[0].cached_path.clone();
        let saved = scene.to_json().expect("serialize resolved scene");
        let mut reopened = Scene::from_json(&saved).expect("reload saved scene");
        resolve_scene_asset_paths(&mut reopened, &scene_parent);

        assert_eq!(reopened.elements[0].path, first_path);
        assert_eq!(reopened.elements[0].cached_path, first_cache);
        assert!(Path::new(&reopened.elements[0].path).is_absolute());
    }

    #[test]
    fn canvas_wheel_zoom_keeps_pointer_anchor_at_non_unit_scale() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        app.scene.current_scale = 1.5;
        app.scene.translate = model::Translate { x: 70.0, y: -20.0 };
        let context = EguiContext::default();
        let pointer = Pos2::new(400.0, 300.0);
        let initial_scale = app.scene.current_scale;
        let initial_translate = app.scene.translate;
        let events = vec![
            egui::Event::PointerMoved(pointer),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, 200.0),
                modifiers: egui::Modifiers::default(),
            },
        ];
        canvas_frame(&context, &mut app, events);

        assert!(app.scene.current_scale > initial_scale);
        let before = Vec2::new(
            (pointer.x - initial_translate.x) / initial_scale,
            (pointer.y - initial_translate.y) / initial_scale,
        );
        let after = Vec2::new(
            (pointer.x - app.scene.translate.x) / app.scene.current_scale,
            (pointer.y - app.scene.translate.y) / app.scene.current_scale,
        );
        assert!((before - after).length() < 0.01);
    }

    #[test]
    fn canvas_drag_moves_item_by_world_delta_at_non_unit_scale() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        app.scene.current_scale = 2.0;
        app.scene.translate = model::Translate { x: 20.0, y: 40.0 };
        let item = Element::new("drag me".to_owned(), "text", 50.0, 60.0, 100.0, 80.0);
        let id = item.id;
        app.scene.elements.push(item);
        let context = EguiContext::default();
        let start = Pos2::new(220.0, 240.0);
        let end = Pos2::new(250.0, 210.0);
        canvas_frame(&context, &mut app, Vec::new());
        canvas_frame(
            &context,
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        assert!(context.input(|input| input.pointer.button_down(egui::PointerButton::Primary)));
        canvas_frame(&context, &mut app, vec![egui::Event::PointerMoved(end)]);
        assert!(
            app.selected.is_some(),
            "drag input did not hit the video/text item"
        );
        canvas_frame(
            &context,
            &mut app,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
        );

        let moved = app
            .scene
            .elements
            .iter()
            .find(|element| element.id == id)
            .unwrap();
        assert!((moved.x - 65.0).abs() < 0.1, "unexpected x {}", moved.x);
        assert!((moved.y - 45.0).abs() < 0.1, "unexpected y {}", moved.y);
    }

    #[test]
    fn ctrl_s_event_atomically_saves_existing_scene_layout_and_text() {
        let directory = tempfile::tempdir().expect("create scene temp directory");
        let path = directory.path().join("layout.purgif");
        let mut app = app_for_shortcut_test(path.clone());
        let mut text = Element::new("第一行\n第二行".to_owned(), "text", 31.0, 47.0, 325.0, 85.0);
        text.width = Some(325.0);
        text.height = Some(85.0);
        app.scene.elements.push(text);

        let context = EguiContext::default();
        let press_modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let input = egui::RawInput {
            modifiers: egui::Modifiers::default(),
            events: vec![
                egui::Event::Key {
                    key: Key::S,
                    physical_key: Some(Key::S),
                    pressed: true,
                    repeat: false,
                    modifiers: press_modifiers,
                },
                egui::Event::Key {
                    key: Key::S,
                    physical_key: Some(Key::S),
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let _ = context.run(input, |ctx| app.handle_shortcuts(ctx));

        assert_eq!(app.status, format!("Saved {}", path.display()));
        let saved = Scene::from_json(&fs::read_to_string(path).expect("read saved scene"))
            .expect("parse saved scene");
        let saved_text = saved
            .elements
            .iter()
            .find(|element| element.media_type == "text")
            .expect("saved text item");
        assert_eq!(saved_text.path, "第一行\n第二行");
        assert_eq!((saved_text.x, saved_text.y), (31.0, 47.0));
        assert_eq!(
            (saved_text.width, saved_text.height),
            (Some(325.0), Some(85.0))
        );
    }
}
