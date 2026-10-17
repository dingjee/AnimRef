#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_action;
mod dialog_coordinator;
mod icon;
mod window_host;

use animref::{asset_io, media, model, pinterest, youtube};
use anyhow::{Context, Result};
use app_action::AppAction;
#[cfg(windows)]
use clipboard_win::Getter;
use dialog_coordinator::{DialogCoordinator, DialogRequest, DialogResult};
use eframe::egui::{
    self, Color32, ColorImage, Context as EguiContext, FontId, Key, Pos2, Rect, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2, ViewportCommand, WindowLevel,
};
use image::GenericImageView;
use model::{Element, Scene};
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use youtube::{DownloadEvent, DownloadJob, DownloadProgress, DownloadStage, VideoMetadata};

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
    let size = cli.window_size.unwrap_or([900.0, 650.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([320.0, 220.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(crate::icon::viewport_icon())
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
    ui_scenario: UiScenario,
    window_size: Option<[f32; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum UiScenario {
    #[default]
    Clean,
    Title,
    Menu,
    Playback,
    Youtube,
    Grid,
    ItemMenu,
    WindowMenu,
    RecentMenu,
    Toast,
    Resize,
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
            if arg == "--ui-scenario" {
                if let Some(value) = args.next() {
                    options.ui_scenario = match value.to_string_lossy().as_ref() {
                        "title" => UiScenario::Title,
                        "menu" => UiScenario::Menu,
                        "playback" => UiScenario::Playback,
                        "youtube" => UiScenario::Youtube,
                        "grid" => UiScenario::Grid,
                        "item-menu" => UiScenario::ItemMenu,
                        "window-menu" => UiScenario::WindowMenu,
                        "recent-menu" => UiScenario::RecentMenu,
                        "toast" => UiScenario::Toast,
                        "resize" => UiScenario::Resize,
                        _ => UiScenario::Clean,
                    };
                }
                continue;
            }
            if arg == "--window-size" {
                let value = args.next();
                if let Some(value) = value.as_ref()
                    && let Some((width, height)) = value.to_string_lossy().split_once('x')
                    && let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>())
                    && width >= 320.0
                    && height >= 220.0
                    && width.is_finite()
                    && height.is_finite()
                {
                    options.window_size = Some([width, height]);
                } else {
                    let value = value
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    eprintln!(
                        "Ignoring invalid --window-size {:?}; expected WIDTHxHEIGHT with width >= 320 and height >= 220",
                        value
                    );
                }
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

struct YoutubeDialog {
    url: String,
    metadata: Option<VideoMetadata>,
    metadata_waiting: bool,
    metadata_job: Option<youtube::MetadataJob>,
    start_text: String,
    end_text: String,
    progress: Option<DownloadProgress>,
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
            progress: None,
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
    show_youtube: Option<YoutubeDialog>,
    show_text_dialog: bool,
    text_draft: String,
    image_assets: HashMap<String, TextureHandle>,
    image_errors: HashMap<String, String>,
    remote_jobs: Vec<crossbeam_channel::Receiver<RemoteImageResult>>,
    pinterest_jobs: Vec<(u64, pinterest::PinterestJob)>,
    remote_pending: HashSet<u64>,
    scene_generation: u64,
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
    actions: VecDeque<AppAction>,
    dialogs: DialogCoordinator,
    toast: Option<(String, Instant)>,
    context_menu_pos: Option<Pos2>,
    context_menu_item: bool,
    context_submenu: Option<&'static str>,
    context_submenu_anchor: Option<Rect>,
    diagnostic_menu_rect: Option<Rect>,
    diagnostic_transport_rect: Option<Rect>,
    diagnostic_header_rect: Option<Rect>,
    diagnostic_toast_rect: Option<Rect>,
    resize_hovered: bool,
    hovered_header: bool,
    last_presented_position: HashMap<u64, f64>,
    animation_paused: HashMap<u64, bool>,
    texture_paths: HashMap<u64, String>,
    playback_sessions: HashMap<u64, media::PlaybackSession>,
    playback_textures: HashMap<u64, TextureHandle>,
    playback_errors: HashMap<u64, String>,
    expected_playback_epoch: HashMap<u64, u64>,
    presented_playback: HashMap<u64, media::PlaybackSnapshot>,
    pending_seeks: HashMap<u64, f64>,
    playback_scenario_pending: bool,
    observed_status: String,
    window_host: window_host::WindowHost,
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
            status: String::new(),
            always_on_top: true,
            show_youtube: None,
            show_text_dialog: false,
            text_draft: String::new(),
            image_assets: HashMap::new(),
            image_errors: HashMap::new(),
            remote_jobs: Vec::new(),
            pinterest_jobs: Vec::new(),
            remote_pending: HashSet::new(),
            scene_generation: 1,
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
            actions: VecDeque::new(),
            dialogs: DialogCoordinator::default(),
            toast: None,
            context_menu_pos: None,
            context_menu_item: false,
            context_submenu: None,
            context_submenu_anchor: None,
            diagnostic_menu_rect: None,
            diagnostic_transport_rect: None,
            diagnostic_header_rect: None,
            diagnostic_toast_rect: None,
            resize_hovered: false,
            hovered_header: false,
            last_presented_position: HashMap::new(),
            animation_paused: HashMap::new(),
            texture_paths: HashMap::new(),
            playback_sessions: HashMap::new(),
            playback_textures: HashMap::new(),
            playback_errors: HashMap::new(),
            expected_playback_epoch: HashMap::new(),
            presented_playback: HashMap::new(),
            pending_seeks: HashMap::new(),
            playback_scenario_pending: false,
            observed_status: String::new(),
            window_host: window_host::WindowHost::default(),
        };
        if let Some(scene_path) = app.cli.scene_path.clone() {
            if let Err(error) = app.load_scene(&scene_path) {
                app.status = format!("Could not load {}: {error:#}", scene_path.display());
                eprintln!("AnimRef smoke scene load failed: {error:#}");
            }
        } else if let Some(last) = app.recent.last().cloned() {
            let _ = app.load_scene(&last);
        }
        match app.cli.ui_scenario {
            UiScenario::Youtube => {
                let mut dialog = YoutubeDialog::new(
                    "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
                    None,
                    app.scene_generation,
                );
                dialog.metadata = Some(VideoMetadata {
                    id: "jNQXAC9IVRw".into(),
                    title: "Me at the zoo — a deliberately long diagnostic title for responsive layout checks".into(),
                    duration: Some(19.0),
                });
                dialog.start_text = "4".into();
                dialog.end_text = "6".into();
                app.show_youtube = Some(dialog);
            }
            UiScenario::Menu => app.context_menu_pos = Some(Pos2::new(24.0, 58.0)),
            UiScenario::Title => app.hovered_header = true,
            UiScenario::Grid => app.scene.grid.enabled = true,
            UiScenario::ItemMenu => {
                app.selected = app.scene.elements.len().checked_sub(1);
                app.context_menu_pos = Some(Pos2::new(24.0, 58.0));
                app.context_menu_item = true;
            }
            UiScenario::WindowMenu | UiScenario::RecentMenu => {
                app.context_menu_pos = Some(Pos2::new(24.0, 58.0));
                app.context_submenu = Some(if app.cli.ui_scenario == UiScenario::WindowMenu {
                    "Window"
                } else {
                    "Recent"
                });
            }
            UiScenario::Toast => app.status = "Changes saved successfully".into(),
            UiScenario::Resize => app.selected = (!app.scene.elements.is_empty()).then_some(0),
            UiScenario::Playback => app.playback_scenario_pending = true,
            UiScenario::Clean => {}
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
        self.cancel_pinterest_jobs();
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
        self.clear_media_caches();
        self.add_recent(scene_path.clone());
        self.status = format!("Loaded {}", scene_path.display());
        Ok(())
    }

    fn save_scene(&mut self) -> Result<()> {
        if let Some(path) = self.scene_path.clone() {
            self.save_scene_to(path)
        } else {
            self.dialogs.enqueue(DialogRequest::SaveScene);
            Ok(())
        }
    }

    fn save_scene_as(&mut self) -> Result<()> {
        self.dialogs.enqueue(DialogRequest::SaveScene);
        Ok(())
    }

    fn save_scene_to(&mut self, path: PathBuf) -> Result<()> {
        let mut scene_to_save = self.scene.clone();
        for element in &mut scene_to_save.elements {
            if let Some(position) = self.last_presented_position.get(&element.id) {
                element.playback_state.position_seconds = *position;
            }
            if let Some(paused) = self.animation_paused.get(&element.id) {
                element.playback_state.paused = *paused;
            }
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
        self.cancel_pinterest_jobs();
        self.scene_generation = self.scene_generation.wrapping_add(1);
        self.scene = Scene::default();
        self.scene_path = None;
        self.selected = None;
        self.clear_media_caches();
        self.status = "New scene".into();
    }

    fn clear_media_caches(&mut self) {
        self.pending_seeks.clear();
        self.playback_sessions.clear();
        self.playback_textures.clear();
        self.playback_errors.clear();
        self.expected_playback_epoch.clear();
        self.presented_playback.clear();
        self.last_presented_position.clear();
        self.animation_paused.clear();
        self.image_assets.clear();
        self.image_errors.clear();
        self.texture_paths.clear();
    }

    fn clear_item_cache(&mut self, id: u64) {
        self.pending_seeks.remove(&id);
        let key = id.to_string();
        self.image_assets.remove(&key);
        self.image_errors.remove(&key);
        self.texture_paths.remove(&id);
        self.playback_sessions.remove(&id);
        self.playback_textures.remove(&id);
        self.playback_errors.remove(&id);
        self.presented_playback.remove(&id);
        self.last_presented_position.remove(&id);
        self.animation_paused.remove(&id);
        self.expected_playback_epoch.remove(&id);
        self.remote_pending.remove(&id);
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

    fn start_pinterest_download(&mut self, url: String) {
        match pinterest::begin_download(url) {
            Ok(job) => {
                self.pinterest_jobs.push((self.scene_generation, job));
                self.status = "Downloading Pinterest Pin…".into();
            }
            Err(error) => self.status = format!("Could not import Pinterest Pin: {error}"),
        }
    }

    fn cancel_pinterest_jobs(&mut self) {
        for (_, job) in self.pinterest_jobs.drain(..) {
            job.cancel();
        }
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
                if is_pinterest_pin_url(&text) {
                    self.start_pinterest_download(text);
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

    fn drain_pinterest_imports(&mut self) {
        let mut pending = Vec::new();
        let jobs = std::mem::take(&mut self.pinterest_jobs);
        for (generation, job) in jobs {
            loop {
                match job.events.try_recv() {
                    Ok(pinterest::PinterestEvent::Progress(message)) => {
                        if generation == self.scene_generation {
                            self.status = message;
                        }
                    }
                    Ok(pinterest::PinterestEvent::Finished(media)) => {
                        if generation == self.scene_generation {
                            self.finish_pinterest_download(media);
                        }
                        break;
                    }
                    Ok(pinterest::PinterestEvent::Failed(error)) => {
                        if generation == self.scene_generation {
                            self.status = format!("Pinterest import failed: {error}");
                        }
                        break;
                    }
                    Ok(pinterest::PinterestEvent::Cancelled) => break,
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        pending.push((generation, job));
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        if generation == self.scene_generation {
                            self.status = "Pinterest download stopped unexpectedly".into();
                        }
                        break;
                    }
                }
            }
        }
        self.pinterest_jobs = pending;
    }

    fn finish_pinterest_download(&mut self, media: pinterest::PinterestMedia) {
        let kind = match media.media_kind {
            pinterest::PinterestMediaKind::Image => "img",
            pinterest::PinterestMediaKind::Video => "video",
        };
        let natural = Vec2::new(media.width.max(1) as f32, media.height.max(1) as f32);
        let scale = (640.0 / natural.x).min(480.0 / natural.y).min(1.0);
        let width = (natural.x * scale).max(10.0);
        let height = (natural.y * scale).max(10.0);
        let position = 100.0 + self.scene.elements.len() as f32 * 20.0;
        let local_path = media.local_path;
        let mut element = Element::new(
            local_path.to_string_lossy().into_owned(),
            kind,
            position,
            position,
            width,
            height,
        );
        element.source_url = Some(media.original_url);
        element.cached_path = Some(local_path);
        element.media_duration = media.duration;
        self.scene.elements.push(element);
        self.selected = self.scene.elements.len().checked_sub(1);
        self.status = match media.media_kind {
            pinterest::PinterestMediaKind::Image => "Pinterest image added".into(),
            pinterest::PinterestMediaKind::Video => "Pinterest video added".into(),
        };
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
                || self.image_errors.contains_key(&key)
                || self.texture_paths.contains_key(&element.id)
            {
                continue;
            }
            let prepared = match asset_io::prepare(element) {
                Ok(prepared) => prepared,
                Err(error) => {
                    if !(element.is_video() && is_http_url(&element.path)) {
                        self.image_errors.insert(key, error.to_string());
                    }
                    continue;
                }
            };
            match prepared.kind {
                asset_io::AssetKind::Video => {
                    if !element.is_video() {
                        self.image_errors
                            .insert(key, "A video file must be imported as video media.".into());
                        continue;
                    }
                }
                asset_io::AssetKind::Gif => {
                    let path = prepared.path.to_string_lossy().into_owned();
                    self.texture_paths.insert(element.id, path);
                    match asset_io::image_rgba(&prepared) {
                        Ok(rgba) => intrinsic_sizes.push((
                            element.id,
                            rgba.width() as f32,
                            rgba.height() as f32,
                        )),
                        Err(error) => {
                            self.image_errors.insert(key, error.to_string());
                        }
                    }
                }
                asset_io::AssetKind::Image => match asset_io::image_rgba(&prepared) {
                    Ok(rgba) => {
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
                },
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
        self.start_playback_sessions();
        if self.playback_scenario_pending
            && let Some(index) = self
                .scene
                .elements
                .iter()
                .position(|element| self.playback_sessions.contains_key(&element.id))
        {
            self.selected = Some(index);
            self.playback_scenario_pending = false;
        }
        self.drain_playback_frames(ctx);
    }

    fn start_playback_sessions(&mut self) {
        for element in &self.scene.elements {
            if self.playback_sessions.contains_key(&element.id)
                || self.playback_errors.contains_key(&element.id)
                || self.image_errors.contains_key(&element.id.to_string())
            {
                continue;
            }
            if element.media_type == "youtube" && cached_youtube_path(element).is_none() {
                continue;
            }
            let prepared_path = if let Some(path) = self.texture_paths.get(&element.id) {
                Some((PathBuf::from(path), media::MediaKind::Gif))
            } else if element.media_type == "youtube" {
                // A YouTube webpage URL is never a decoder input. Use its local section cache.
                if cached_youtube_path(element).is_some() {
                    match asset_io::prepare(element) {
                        Ok(prepared) if prepared.kind == asset_io::AssetKind::Video => {
                            Some((prepared.path, media::MediaKind::Video))
                        }
                        Ok(prepared) if prepared.kind == asset_io::AssetKind::Gif => {
                            self.texture_paths
                                .insert(element.id, prepared.path.to_string_lossy().into_owned());
                            Some((prepared.path, media::MediaKind::Gif))
                        }
                        Ok(_) => None,
                        Err(error) => {
                            self.playback_errors.insert(element.id, error.to_string());
                            None
                        }
                    }
                } else {
                    None
                }
            } else if let Some(url) = remote_playback_url(element) {
                Some((PathBuf::from(url), media::MediaKind::Video))
            } else if element.is_video() {
                match asset_io::prepare(element) {
                    Ok(prepared) if prepared.kind == asset_io::AssetKind::Video => {
                        Some((prepared.path, media::MediaKind::Video))
                    }
                    Ok(prepared) if prepared.kind == asset_io::AssetKind::Gif => {
                        self.texture_paths
                            .insert(element.id, prepared.path.to_string_lossy().into_owned());
                        Some((prepared.path, media::MediaKind::Gif))
                    }
                    Ok(_) => None,
                    Err(error) => {
                        self.playback_errors.insert(element.id, error.to_string());
                        None
                    }
                }
            } else {
                None
            };
            let Some((path, kind)) = prepared_path else {
                continue;
            };
            let duration = if element.media_type == "youtube" {
                element
                    .media_duration
                    .or_else(|| element.source_range.map(|r| r[1] - r[0]))
            } else {
                element.media_duration
            };
            let pairs =
                if element.media_type == "youtube" && !element.playback_loop_pairs.is_empty() {
                    element.playback_loop_pairs.clone()
                } else {
                    element.loop_pairs.clone()
                };
            let active = if element.media_type == "youtube" {
                element.active_playback_pair()
            } else {
                element.active_loop_pair
            };
            let session = media::PlaybackSession::start(
                path,
                kind,
                duration,
                pairs,
                active,
                element.playback_state,
            );
            self.expected_playback_epoch
                .insert(element.id, session.snapshot().epoch);
            self.animation_paused
                .insert(element.id, session.snapshot().paused);
            self.playback_sessions.insert(element.id, session);
        }
    }

    fn command_playback(&mut self, id: u64, command: media::PlaybackCommand) {
        if let Some(session) = self.playback_sessions.get(&id) {
            let current_epoch = self
                .expected_playback_epoch
                .get(&id)
                .copied()
                .unwrap_or_else(|| session.snapshot().epoch);
            self.expected_playback_epoch
                .insert(id, current_epoch.wrapping_add(1));
            let previous_paused = self
                .animation_paused
                .get(&id)
                .copied()
                .unwrap_or(session.snapshot().paused);
            let paused = match command {
                media::PlaybackCommand::Play => false,
                media::PlaybackCommand::Pause
                | media::PlaybackCommand::StepForward
                | media::PlaybackCommand::StepBackward => true,
                media::PlaybackCommand::Toggle => !previous_paused,
                media::PlaybackCommand::Seek(_) | media::PlaybackCommand::Stop => previous_paused,
            };
            self.animation_paused.insert(id, paused);
            session.command(command);
        }
    }

    fn drain_playback_frames(&mut self, ctx: &EguiContext) {
        let ids: Vec<_> = self.playback_sessions.keys().copied().collect();
        let mut received = false;
        for id in ids {
            let Some(session) = self.playback_sessions.get(&id) else {
                continue;
            };
            while let Ok(event) = session.events.try_recv() {
                match event {
                    media::PlaybackEvent::Frame {
                        epoch,
                        position_seconds,
                        frame_index,
                        pixels,
                    } => {
                        if self
                            .expected_playback_epoch
                            .get(&id)
                            .is_some_and(|expected| *expected != epoch)
                        {
                            continue;
                        }
                        let decoded = match pixels {
                            media::DecodedPixels::Jpeg(bytes) => image::load_from_memory(&bytes)
                                .ok()
                                .map(|img| img.to_rgba8()),
                            media::DecodedPixels::Rgba {
                                width,
                                height,
                                bytes,
                            } => image::RgbaImage::from_raw(width, height, bytes),
                        };
                        match decoded {
                            Some(rgba) => {
                                let color = ColorImage::from_rgba_unmultiplied(
                                    [rgba.width() as usize, rgba.height() as usize],
                                    rgba.as_raw(),
                                );
                                if let Some(texture) = self.playback_textures.get_mut(&id) {
                                    texture.set(color, TextureOptions::LINEAR);
                                } else {
                                    self.playback_textures.insert(
                                        id,
                                        ctx.load_texture(
                                            format!("playback-{id}"),
                                            color,
                                            TextureOptions::LINEAR,
                                        ),
                                    );
                                }
                                let mut snapshot = session.snapshot();
                                snapshot.position_seconds = position_seconds;
                                snapshot.frame_index = frame_index;
                                self.presented_playback.insert(id, snapshot);
                                self.last_presented_position.insert(id, position_seconds);
                                self.decoded_video_frames =
                                    self.decoded_video_frames.saturating_add(1);
                                *self.decoded_video_frames_by_id.entry(id).or_default() += 1;
                                received = true;
                            }
                            None => {
                                self.playback_errors
                                    .insert(id, "Could not decode playback frame".into());
                            }
                        }
                    }
                    media::PlaybackEvent::Failed(error) => {
                        self.playback_errors.insert(id, error);
                    }
                }
            }
        }
        if received && self.smoke_report_written {
            self.write_smoke_report(ctx);
        }
    }

    fn queue_action(&mut self, action: AppAction) {
        self.actions.push_back(action);
        self.context_menu_pos = None;
        self.context_menu_item = false;
        self.context_submenu = None;
        self.context_submenu_anchor = None;
    }

    fn run_actions(&mut self, ctx: &EguiContext) {
        while let Some(action) = self.actions.pop_front() {
            match action {
                AppAction::NewScene => self.new_scene(),
                AppAction::OpenScene => self.dialogs.enqueue(DialogRequest::OpenScene),
                AppAction::OpenRecent(path) => {
                    let result = self.load_scene(&path);
                    self.capture_result(result);
                }
                AppAction::Save => {
                    let result = self.save_scene();
                    self.capture_result(result);
                }
                AppAction::SaveAs => {
                    let result = self.save_scene_as();
                    self.capture_result(result);
                }
                AppAction::ImportMedia => self.dialogs.enqueue(DialogRequest::ImportMedia),
                AppAction::ExportSelected => self.export_selected(),
                AppAction::Paste => self.handle_paste(),
                AppAction::CopySelected => self.copy_selected(),
                AppAction::DeleteSelected => self.delete_selected(),
                AppAction::BringForward => self.move_selected_layer(1),
                AppAction::SendBackward => self.move_selected_layer(-1),
                AppAction::AddText => self.show_text_dialog = true,
                AppAction::PrepareYoutube { url, target_id } => {
                    self.start_youtube_metadata(url, target_id)
                }
                AppAction::OpenSourceLink(url) => {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        ctx.open_url(egui::OpenUrl::new_tab(url));
                    }
                }
                AppAction::ToggleGrid => {
                    self.scene.grid.enabled = !self.scene.grid.enabled;
                }
                AppAction::CenterView => self.scene.translate = model::Translate::default(),
                AppAction::FitView => self.fit_scene(ctx),
                AppAction::SetLoopStart => self.set_loop_boundary(true),
                AppAction::SetLoopEnd => self.set_loop_boundary(false),
                AppAction::ResetLoop => self.reset_active_loop(),
                AppAction::TogglePin => {
                    self.always_on_top = !self.always_on_top;
                    ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.always_on_top {
                        WindowLevel::AlwaysOnTop
                    } else {
                        WindowLevel::Normal
                    }));
                }
                AppAction::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                AppAction::ToggleMaximize => {
                    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                AppAction::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
                AppAction::PlayPause => {
                    if let Some(id) = self.selected_id() {
                        self.command_playback(id, media::PlaybackCommand::Toggle);
                    }
                }
                AppAction::StepAnimation(direction) => {
                    if let Some(id) = self.selected_id() {
                        self.command_playback(
                            id,
                            if direction < 0 {
                                media::PlaybackCommand::StepBackward
                            } else {
                                media::PlaybackCommand::StepForward
                            },
                        );
                    }
                }
                AppAction::SeekAnimation(position) => {
                    if let Some(id) = self.selected_id() {
                        self.command_playback(id, media::PlaybackCommand::Seek(position));
                    }
                }
            }
        }
    }

    fn selected_id(&self) -> Option<u64> {
        self.selected
            .and_then(|index| self.scene.elements.get(index))
            .map(|element| element.id)
    }

    fn delete_selected(&mut self) {
        if let Some(index) = self
            .selected
            .take()
            .filter(|index| *index < self.scene.elements.len())
        {
            let removed = self.scene.elements.remove(index);
            self.clear_item_cache(removed.id);
        }
    }

    fn move_selected_layer(&mut self, direction: isize) {
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            return;
        };
        let next =
            (index as isize + direction).clamp(0, self.scene.elements.len() as isize - 1) as usize;
        if next != index {
            let element = self.scene.elements.remove(index);
            self.scene.elements.insert(next, element);
            self.selected = Some(next);
        }
    }

    fn set_loop_boundary(&mut self, start: bool) {
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            return;
        };
        let id = self.scene.elements[index].id;
        self.persist_playback_state(id);
        let state = self.presented_playback.get(&id).copied().or_else(|| {
            self.playback_sessions
                .get(&id)
                .map(media::PlaybackSession::snapshot)
        });
        let Some(state) = state else {
            return;
        };
        let Some(duration) = state.duration_seconds.filter(|duration| *duration > 0.0) else {
            return;
        };
        let position = (state.position_seconds / duration * 100.0).clamp(0.0, 100.0);
        let element = &mut self.scene.elements[index];
        let active_index = element.active_playback_pair();
        let pairs = if element.media_type == "youtube" && !element.playback_loop_pairs.is_empty() {
            &mut element.playback_loop_pairs
        } else {
            &mut element.loop_pairs
        };
        if pairs.is_empty() {
            pairs.push([0.0, 100.0]);
        }
        let active = active_index.min(pairs.len() - 1);
        if start {
            pairs[active][0] = position.min((pairs[active][1] - 0.1).max(0.0));
        } else {
            pairs[active][1] = position.max((pairs[active][0] + 0.1).min(100.0));
        }
        self.playback_sessions.remove(&id);
        self.playback_textures.remove(&id);
        self.presented_playback.remove(&id);
        self.last_presented_position.remove(&id);
    }

    fn reset_active_loop(&mut self) {
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            return;
        };
        let id = self.scene.elements[index].id;
        self.persist_playback_state(id);
        let element = &mut self.scene.elements[index];
        let active_index = element.active_playback_pair();
        let pairs = if element.media_type == "youtube" && !element.playback_loop_pairs.is_empty() {
            &mut element.playback_loop_pairs
        } else {
            &mut element.loop_pairs
        };
        if pairs.is_empty() {
            pairs.push([0.0, 100.0]);
        }
        let active = active_index.min(pairs.len() - 1);
        pairs[active] = [0.0, 100.0];
        self.playback_sessions.remove(&id);
        self.playback_textures.remove(&id);
        self.presented_playback.remove(&id);
        self.last_presented_position.remove(&id);
    }

    fn persist_playback_state(&mut self, id: u64) {
        let position = self.last_presented_position.get(&id).copied();
        let paused = self.animation_paused.get(&id).copied();
        if let Some(element) = self
            .scene
            .elements
            .iter_mut()
            .find(|element| element.id == id)
        {
            if let Some(position) = position {
                element.playback_state.position_seconds = position;
            }
            if let Some(paused) = paused {
                element.playback_state.paused = paused;
            }
        }
    }

    fn fit_scene(&mut self, ctx: &EguiContext) {
        if self.scene.elements.is_empty() {
            self.scene.current_scale = 1.0;
            self.scene.translate = model::Translate::default();
            return;
        }
        let min_x = self
            .scene
            .elements
            .iter()
            .map(|element| element.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = self
            .scene
            .elements
            .iter()
            .map(|element| element.y)
            .fold(f32::INFINITY, f32::min);
        let max_x = self
            .scene
            .elements
            .iter()
            .map(|element| element.x + element.width.unwrap_or(320.0))
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = self
            .scene
            .elements
            .iter()
            .map(|element| element.y + element.height.unwrap_or(200.0))
            .fold(f32::NEG_INFINITY, f32::max);
        let bounds = Vec2::new((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
        let view = ctx.screen_rect().size() - Vec2::splat(64.0);
        self.scene.current_scale = (view.x / bounds.x).min(view.y / bounds.y).clamp(0.01, 2.0);
        self.scene.translate.x = (view.x - bounds.x * self.scene.current_scale) * 0.5
            - min_x * self.scene.current_scale
            + 32.0;
        self.scene.translate.y = (view.y - bounds.y * self.scene.current_scale) * 0.5
            - min_y * self.scene.current_scale
            + 32.0;
    }

    fn copy_selected(&mut self) {
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            return;
        };
        let element = self.scene.elements[index].clone();
        if element.media_type == "text" {
            self.set_clipboard_text(&element.path);
            return;
        }
        match asset_io::prepare(&element).and_then(|prepared| match prepared.kind {
            asset_io::AssetKind::Video | asset_io::AssetKind::Gif => {
                self.set_clipboard_file(&prepared.path);
                Ok(())
            }
            asset_io::AssetKind::Image => {
                let rgba = asset_io::image_rgba(&prepared)?;
                let payload = arboard::ImageData {
                    width: rgba.width() as usize,
                    height: rgba.height() as usize,
                    bytes: std::borrow::Cow::Owned(rgba.into_raw()),
                };
                arboard::Clipboard::new()?
                    .set_image(payload)
                    .map_err(anyhow::Error::from)?;
                self.status = "Image copied".into();
                Ok(())
            }
        }) {
            Ok(()) => {}
            Err(error) => self.status = format!("Could not copy selected media: {error:#}"),
        }
    }

    fn set_clipboard_text(&mut self, text: &str) {
        match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
        {
            Ok(()) => self.status = "Text copied".into(),
            Err(error) => self.status = format!("Could not copy text: {error}"),
        }
    }

    fn set_clipboard_file(&mut self, path: &Path) {
        #[cfg(windows)]
        {
            use clipboard_win::Setter;
            let path = path.to_string_lossy().into_owned();
            match clipboard_win::formats::FileList.write_clipboard(&[path]) {
                Ok(()) => self.status = "File copied".into(),
                Err(error) => self.status = format!("Could not copy file: {error}"),
            }
        }
        #[cfg(not(windows))]
        self.set_clipboard_text(&path.to_string_lossy());
    }

    fn export_selected(&mut self) {
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            return;
        };
        let element = self.scene.elements[index].clone();
        if element.media_type == "text" {
            self.dialogs.enqueue(DialogRequest::SaveText {
                contents: element.path,
                suggested_name: "text.txt".into(),
            });
            return;
        }
        match asset_io::prepare(&element) {
            Ok(prepared) => self.dialogs.enqueue(DialogRequest::ExportFile {
                source: prepared.path,
                suggested_name: prepared.suggested_name,
            }),
            Err(error) => self.status = format!("Could not prepare media for export: {error:#}"),
        }
    }

    fn handle_dialog_result(&mut self, result: DialogResult) {
        match result {
            DialogResult::OpenScene(Some(path)) => {
                let result = self.load_scene(&path);
                self.capture_result(result);
            }
            DialogResult::OpenScene(None)
            | DialogResult::SaveScene(None)
            | DialogResult::ImportMedia(None) => {}
            DialogResult::SaveScene(Some(path)) => {
                let result = self.save_scene_to(path);
                self.capture_result(result);
            }
            DialogResult::ImportMedia(Some(paths)) => {
                for path in paths {
                    self.import_path(&path);
                }
            }
            DialogResult::ExportFile {
                source,
                destination: Some(destination),
            } => {
                let same_file = matches!(
                    (fs::canonicalize(&source), fs::canonicalize(&destination)),
                    (Ok(source), Ok(destination)) if source == destination
                );
                if same_file {
                    self.status = "Selected destination already contains this file".into();
                } else if let Err(error) = copy_file_atomically(&source, &destination) {
                    self.status = format!("Could not export {}: {error}", source.display());
                } else {
                    self.status = format!("Exported {}", destination.display());
                }
            }
            DialogResult::ExportFile {
                destination: None, ..
            } => {}
            DialogResult::SaveText {
                contents,
                destination: Some(destination),
            } => {
                if let Err(error) = write_file_atomically(&destination, contents.as_bytes()) {
                    self.status = format!("Could not export text: {error}");
                } else {
                    self.status = format!("Exported {}", destination.display());
                }
            }
            DialogResult::SaveText {
                destination: None, ..
            } => {}
        }
    }

    fn draw_hover_header(&mut self, ctx: &EguiContext) {
        self.diagnostic_header_rect = None;
        let pointer = ctx.input(|input| input.pointer.hover_pos());
        let show =
            self.cli.ui_scenario == UiScenario::Title || pointer.is_some_and(|pos| pos.y <= 42.0);
        self.hovered_header = show;
        if !show {
            return;
        }
        let title = self
            .scene_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let width = ctx.screen_rect().width();
        egui::Area::new(egui::Id::new("floating-header"))
            .fixed_pos(Pos2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(32, 35, 41, 245))
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        ui.set_width((width - 20.0).max(200.0));
                        ui.horizontal(|ui| {
                            let title_width = (ui.available_width() - 132.0).max(56.0);
                            let (title_rect, drag) =
                                ui.allocate_exact_size(Vec2::new(title_width, 25.0), Sense::drag());
                            let drag = drag.on_hover_text(
                                self.scene_path
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| "Unsaved scene".into()),
                            );
                            let title_color = ui.visuals().text_color();
                            let title_font = egui::FontId::proportional(16.0);
                            let title_center = width * 0.5;
                            let text_right = (title_rect.right() - 8.0).min(width - 10.0);
                            let half_text_width = (title_center - title_rect.left())
                                .min(text_right - title_center)
                                .max(1.0);
                            let text_rect = Rect::from_center_size(
                                Pos2::new(title_center, title_rect.center().y),
                                Vec2::new(half_text_width * 2.0, title_rect.height()),
                            );
                            let title_galley = ellipsized_galley(
                                ui.painter(),
                                &title,
                                title_font,
                                title_color,
                                text_rect.width(),
                            );
                            ui.painter().with_clip_rect(text_rect).galley(
                                Pos2::new(
                                    text_rect.center().x - title_galley.size().x * 0.5,
                                    title_rect.center().y - title_galley.size().y * 0.5,
                                ),
                                title_galley,
                                title_color,
                            );
                            if drag.dragged() {
                                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                            }
                            let pin = icon_button(
                                ui,
                                if self.always_on_top {
                                    egui_phosphor::regular::PUSH_PIN
                                } else {
                                    egui_phosphor::regular::PUSH_PIN_SIMPLE
                                },
                                if self.always_on_top {
                                    "Always on top (enabled)"
                                } else {
                                    "Always on top (disabled)"
                                },
                            )
                            .on_hover_text(if self.always_on_top {
                                "Always on top (enabled)"
                            } else {
                                "Always on top (disabled)"
                            });
                            if pin.clicked() {
                                self.queue_action(AppAction::TogglePin);
                            }
                            if icon_button(ui, egui_phosphor::regular::MINUS, "Minimize").clicked()
                            {
                                self.queue_action(AppAction::Minimize);
                            }
                            let maximized =
                                ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                            if icon_button(
                                ui,
                                if maximized {
                                    egui_phosphor::regular::CORNERS_IN
                                } else {
                                    egui_phosphor::regular::CORNERS_OUT
                                },
                                "Maximize / restore",
                            )
                            .clicked()
                            {
                                self.queue_action(AppAction::ToggleMaximize);
                            }
                            if icon_button(ui, egui_phosphor::regular::X, "Close").clicked() {
                                self.queue_action(AppAction::Close);
                            }
                        });
                    });
            });
        self.diagnostic_header_rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 40.0)));
    }

    fn draw_context_menu(&mut self, ctx: &EguiContext) {
        let Some(raw_position) = self.context_menu_pos else {
            self.diagnostic_menu_rect = None;
            return;
        };
        let screen = ctx.screen_rect();
        let width = 212.0_f32.min((screen.width() - 16.0).max(120.0));
        let position = Pos2::new(
            raw_position.x.clamp(
                screen.left() + 8.0,
                (screen.right() - width - 8.0).max(screen.left() + 8.0),
            ),
            raw_position.y.clamp(
                screen.top() + 8.0,
                (screen.bottom() - 250.0).max(screen.top() + 8.0),
            ),
        );
        let mut action = None;
        let mut submenu_anchor = None;
        let mut menu_rect = Rect::NOTHING;
        egui::Area::new(egui::Id::new("canvas-context-menu"))
            .fixed_pos(position)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((screen.height() - 16.0).max(100.0))
                    .show(ui, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(width);
                            ui.set_max_width(width);
                            ui.spacing_mut().interact_size.y = 25.0;
                            ui.spacing_mut().item_spacing.y = 1.0;
                            if self.context_menu_item {
                                let selected = self
                                    .selected
                                    .and_then(|index| self.scene.elements.get(index));
                                let media_not_local = selected.is_some_and(|element| {
                                    (element.media_type == "youtube" || is_http_url(&element.path))
                                        && element
                                            .cached_path
                                            .as_ref()
                                            .is_none_or(|path| !path.exists())
                                });
                                if context_menu_row(ui, "Copy", !media_not_local)
                                    .on_hover_text(if media_not_local {
                                        "This media has no local file to copy or export yet"
                                    } else {
                                        "Copy this item to the system clipboard"
                                    })
                                    .clicked()
                                {
                                    action = Some(AppAction::CopySelected);
                                }
                                if context_menu_row(ui, "Export…", !media_not_local).clicked() {
                                    action = Some(AppAction::ExportSelected);
                                }
                                ui.separator();
                                if context_menu_row(ui, "Bring forward", true).clicked() {
                                    action = Some(AppAction::BringForward);
                                }
                                if context_menu_row(ui, "Send backward", true).clicked() {
                                    action = Some(AppAction::SendBackward);
                                }
                                if context_menu_row(ui, "Delete", true).clicked() {
                                    action = Some(AppAction::DeleteSelected);
                                }
                                if let Some(element) = selected {
                                    if let Some(source_url) = element.source_link()
                                        && context_menu_row(ui, "Open source link", true)
                                            .on_hover_text(source_url.to_owned())
                                            .clicked()
                                    {
                                        action =
                                            Some(AppAction::OpenSourceLink(source_url.to_owned()));
                                    }
                                    let has_duration = element.media_duration.is_some()
                                        || self
                                            .presented_playback
                                            .get(&element.id)
                                            .and_then(|snapshot| snapshot.duration_seconds)
                                            .is_some();
                                    if element.is_video() && has_duration {
                                        let response = context_menu_submenu_row(
                                            ui,
                                            "Playback",
                                            self.context_submenu == Some("Playback"),
                                        );
                                        if response.hovered() || response.clicked() {
                                            submenu_anchor = Some(("Playback", response.rect));
                                        }
                                    }
                                    if element.media_type == "youtube"
                                        && context_menu_row(
                                            ui,
                                            if element
                                                .cached_path
                                                .as_ref()
                                                .is_some_and(|path| path.exists())
                                            {
                                                "Download another section…"
                                            } else {
                                                "Choose section…"
                                            },
                                            true,
                                        )
                                        .clicked()
                                    {
                                        action = Some(AppAction::PrepareYoutube {
                                            url: element.path.clone(),
                                            target_id: Some(element.id),
                                        });
                                    }
                                }
                            } else {
                                for (label, candidate) in [
                                    ("Open…", AppAction::OpenScene),
                                    ("Save", AppAction::Save),
                                    ("Save As…", AppAction::SaveAs),
                                    ("New", AppAction::NewScene),
                                    ("Import media…", AppAction::ImportMedia),
                                    ("Add text…", AppAction::AddText),
                                    ("Paste", AppAction::Paste),
                                ] {
                                    if context_menu_row(ui, label, true).clicked() {
                                        action = Some(candidate);
                                    }
                                }
                                ui.separator();
                                let grid = context_menu_check_row(
                                    ui,
                                    "Grid & snap",
                                    self.scene.grid.enabled,
                                );
                                if grid.clicked() {
                                    action = Some(AppAction::ToggleGrid);
                                }
                                if context_menu_row(ui, "Center view", true).clicked() {
                                    action = Some(AppAction::CenterView);
                                }
                                if context_menu_row(ui, "Fit all", true).clicked() {
                                    action = Some(AppAction::FitView);
                                }
                                let recent = context_menu_submenu_row(
                                    ui,
                                    "Recent",
                                    self.context_submenu == Some("Recent"),
                                );
                                if recent.hovered()
                                    || recent.clicked()
                                    || self.cli.ui_scenario == UiScenario::RecentMenu
                                {
                                    submenu_anchor = Some(("Recent", recent.rect));
                                }
                                let window = context_menu_submenu_row(
                                    ui,
                                    "Window",
                                    self.context_submenu == Some("Window"),
                                );
                                if window.hovered()
                                    || window.clicked()
                                    || self.cli.ui_scenario == UiScenario::WindowMenu
                                {
                                    submenu_anchor = Some(("Window", window.rect));
                                }
                            }
                        });
                    });
                menu_rect = ui.min_rect();
            });
        self.diagnostic_menu_rect = Some(menu_rect);
        let mut submenu_rect = None;
        if let Some((name, row_rect)) = submenu_anchor {
            self.context_submenu = Some(name);
            self.context_submenu_anchor = Some(row_rect);
        }
        if let Some(name) = self.context_submenu {
            let anchor = self.context_submenu_anchor.unwrap_or(menu_rect);
            let submenu_pos = Pos2::new(
                if anchor.right() + width <= screen.right() {
                    anchor.right() - 2.0
                } else {
                    anchor.left() - width + 2.0
                },
                anchor.top().clamp(
                    screen.top() + 8.0,
                    (screen.bottom() - 100.0).max(screen.top() + 8.0),
                ),
            );
            egui::Area::new(egui::Id::new(("context-submenu", name)))
                .fixed_pos(submenu_pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 16.0).max(100.0))
                        .show(ui, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(width);
                                ui.set_max_width(width);
                                ui.spacing_mut().interact_size.y = 25.0;
                                match name {
                                    "Playback" => {
                                        for (label, candidate) in [
                                            ("Set loop start here", AppAction::SetLoopStart),
                                            ("Set loop end here", AppAction::SetLoopEnd),
                                            ("Reset active loop", AppAction::ResetLoop),
                                        ] {
                                            if context_menu_row(ui, label, true).clicked() {
                                                action = Some(candidate);
                                            }
                                        }
                                    }
                                    "Recent" => {
                                        let paths = self.recent.clone();
                                        if paths.is_empty() {
                                            ui.label("No recent scenes");
                                        }
                                        for path in paths.into_iter().rev() {
                                            let label = path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .into_owned();
                                            if context_menu_row(ui, &label, true)
                                                .on_hover_text(path.display().to_string())
                                                .clicked()
                                            {
                                                action = Some(AppAction::OpenRecent(path));
                                            }
                                        }
                                    }
                                    "Window" => {
                                        let pin = context_menu_check_row(
                                            ui,
                                            "Always on top",
                                            self.always_on_top,
                                        );
                                        if pin.clicked() {
                                            action = Some(AppAction::TogglePin);
                                        }
                                        for (label, candidate) in [
                                            ("Maximize / restore", AppAction::ToggleMaximize),
                                            ("Minimize", AppAction::Minimize),
                                            ("Close", AppAction::Close),
                                        ] {
                                            if context_menu_row(ui, label, true).clicked() {
                                                action = Some(candidate);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            });
                        });
                    submenu_rect = Some(ui.min_rect());
                });
        }
        if let Some(action) = action {
            self.queue_action(action);
        }
        let pointer = ctx.input(|input| input.pointer.interact_pos());
        let clicked_elsewhere = ctx.input(|input| input.pointer.any_click())
            && pointer.is_some_and(|point| {
                !menu_rect.contains(point) && !submenu_rect.is_some_and(|rect| rect.contains(point))
            });
        let pointer_in_transition = pointer.is_some_and(|point| {
            submenu_rect.is_some_and(|rect| rect.expand(6.0).contains(point))
                || menu_rect.expand(6.0).contains(point)
        });
        if clicked_elsewhere {
            self.context_menu_pos = None;
            self.context_submenu = None;
            self.context_submenu_anchor = None;
            self.context_menu_item = false;
        } else if submenu_rect.is_none() && !pointer_in_transition {
            self.context_submenu = None;
        }
    }
    fn draw_selection_bar(&mut self, ctx: &EguiContext) {
        self.diagnostic_transport_rect = None;
        let Some(index) = self
            .selected
            .filter(|index| *index < self.scene.elements.len())
        else {
            self.pending_seeks.clear();
            return;
        };
        let element = &self.scene.elements[index];
        let id = element.id;
        self.pending_seeks.retain(|pending_id, _| *pending_id == id);
        let animated = element.is_video() || self.playback_sessions.contains_key(&id);
        if !animated {
            return;
        }
        let mut state =
            self.presented_playback
                .get(&id)
                .copied()
                .unwrap_or(media::PlaybackSnapshot {
                    paused: element.playback_state.paused,
                    position_seconds: element.playback_state.position_seconds,
                    duration_seconds: element.media_duration,
                    frame_index: None,
                    epoch: 0,
                });
        state.paused = self
            .animation_paused
            .get(&id)
            .copied()
            .unwrap_or(state.paused);
        let mut seek = self
            .pending_seeks
            .get(&id)
            .copied()
            .unwrap_or(state.position_seconds);
        let transport =
            transport_overlay_rect(ctx, &self.scene, element, state.duration_seconds.is_some());
        let max_width = transport.width();
        let response = egui::Area::new(egui::Id::new("animation-transport"))
            .fixed_pos(transport.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(max_width);
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(30, 34, 43, 238))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.set_min_width((max_width - 16.0).max(64.0));
                        ui.horizontal(|ui| {
                            if icon_button(ui, egui_phosphor::regular::SKIP_BACK, "Previous frame")
                                .clicked()
                            {
                                self.queue_action(AppAction::StepAnimation(-1));
                            }
                            if icon_button(
                                ui,
                                if state.paused {
                                    egui_phosphor::regular::PLAY
                                } else {
                                    egui_phosphor::regular::PAUSE
                                },
                                if state.paused { "Play" } else { "Pause" },
                            )
                            .clicked()
                            {
                                self.queue_action(AppAction::PlayPause);
                            }
                            if icon_button(ui, egui_phosphor::regular::SKIP_FORWARD, "Next frame")
                                .clicked()
                            {
                                self.queue_action(AppAction::StepAnimation(1));
                            }
                            if let Some(duration) =
                                state.duration_seconds.filter(|duration| *duration > 0.0)
                            {
                                let slider_width = (ui.available_width() - 84.0).clamp(40.0, 180.0);
                                let response = ui.add_sized(
                                    [slider_width, 18.0],
                                    egui::Slider::new(&mut seek, 0.0..=duration)
                                        .show_value(false)
                                        .trailing_fill(true)
                                        .fixed_decimals(2)
                                        .min_decimals(2)
                                        .max_decimals(2),
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{:.1}/{:.1}s",
                                        state.position_seconds, duration
                                    ))
                                    .size(11.0),
                                );
                                let response_has_focus = response.has_focus();
                                let (pointer_down, keyboard_seek) = ctx.input(|input| {
                                    (
                                        input.pointer.button_down(egui::PointerButton::Primary)
                                            && response.hovered(),
                                        response_has_focus
                                            && [
                                                Key::ArrowLeft,
                                                Key::ArrowRight,
                                                Key::ArrowUp,
                                                Key::ArrowDown,
                                                Key::Home,
                                                Key::End,
                                                Key::PageUp,
                                                Key::PageDown,
                                            ]
                                            .into_iter()
                                            .any(|key| input.key_pressed(key)),
                                    )
                                });
                                if response.changed() && pointer_down {
                                    self.pending_seeks.insert(id, seek);
                                }
                                if response.drag_stopped()
                                    || response.clicked()
                                    || (response.changed() && keyboard_seek)
                                {
                                    self.pending_seeks.remove(&id);
                                    self.queue_action(AppAction::SeekAnimation(seek));
                                }
                            } else {
                                ui.label(format!("{:.2}s", state.position_seconds));
                            }
                        });
                    });
            });
        self.diagnostic_transport_rect = Some(response.response.rect);
    }

    fn draw_toast(&mut self, ctx: &EguiContext) {
        self.diagnostic_toast_rect = None;
        if self.status != self.observed_status {
            self.observed_status = self.status.clone();
            if !self.status.is_empty() {
                self.toast = Some((self.status.clone(), Instant::now()));
            }
        }
        let Some((message, created)) = self.toast.as_ref() else {
            return;
        };
        let toast_lifetime = if self.cli.ui_scenario == UiScenario::Toast {
            Duration::from_secs(12)
        } else {
            Duration::from_secs(4)
        };
        if created.elapsed() > toast_lifetime {
            self.toast = None;
            return;
        }
        let screen = ctx.screen_rect();
        let mut toast_y = 42.0;
        if let Some(transport) = self.diagnostic_transport_rect {
            let toast_guess = Rect::from_center_size(
                Pos2::new(screen.center().x, screen.top() + toast_y + 20.0),
                Vec2::new((screen.width() - 32.0).min(360.0), 40.0),
            );
            if toast_guess.intersects(transport) {
                toast_y = if transport.bottom() + 52.0 < screen.bottom() - 8.0 {
                    transport.bottom() + 8.0
                } else {
                    -(screen.bottom() - transport.top() + 8.0)
                };
            }
        }
        let response = egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, toast_y))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width((ctx.screen_rect().width() - 32.0).max(100.0));
                    ui.label(message);
                });
            })
            .response
            .rect;
        self.diagnostic_toast_rect = Some(response);
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
            && self.started_at.elapsed() >= Duration::from_millis(4500)
        {
            self.write_smoke_report(ctx);
        }

        let video_textures_ready = self
            .scene
            .elements
            .iter()
            .filter(|element| element.is_video() || self.texture_paths.contains_key(&element.id))
            .all(|element| self.playback_textures.contains_key(&element.id));
        if !self.screenshot_requested
            && self.cli.screenshot_path.is_some()
            && self.started_at.elapsed() >= Duration::from_millis(4500)
            && (video_textures_ready || self.started_at.elapsed() >= Duration::from_secs(10))
        {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            ctx.request_repaint();
        }

        if self.smoke_report_written && had_input_events {
            self.write_smoke_report(ctx);
        }

        if !self.smoke_report_written
            && self.cli.smoke_report_path.is_some()
            && self.started_at.elapsed() >= Duration::from_millis(4500)
            && (self.cli.screenshot_path.is_none()
                || self.screenshot_result.is_some()
                || self.started_at.elapsed() >= Duration::from_secs(12))
        {
            self.write_smoke_report(ctx);
        }

        let screenshot_succeeded = self.cli.screenshot_path.is_none()
            || self
                .screenshot_result
                .as_ref()
                .is_some_and(|result| result.starts_with("saved "));
        if self.smoke_exit_ready(screenshot_succeeded) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn write_smoke_report(&mut self, ctx: &EguiContext) {
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
                    "sourceUrl": element.source_link(),
                    "mediaDuration": element.media_duration,
                    "decodedFrames": self.decoded_video_frames_by_id.get(&element.id).copied().unwrap_or(0),
                    "playbackState": element.playback_state,
                    "presentedPositionSeconds": self.last_presented_position.get(&element.id).copied(),
                    "paused": self.animation_paused.get(&element.id).copied().unwrap_or(element.playback_state.paused),
                    "imageError": self.image_errors.get(&key),
                    "videoError": self.playback_errors.get(&element.id),
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "processId": std::process::id(),
            "scenePath": self.scene_path,
            "uiScenario": match self.cli.ui_scenario { UiScenario::Clean => "clean", UiScenario::Title => "title", UiScenario::Menu => "menu", UiScenario::Playback => "playback", UiScenario::Youtube => "youtube", UiScenario::Grid => "grid", UiScenario::ItemMenu => "item-menu", UiScenario::WindowMenu => "window-menu", UiScenario::RecentMenu => "recent-menu", UiScenario::Toast => "toast", UiScenario::Resize => "resize" },
            "windowSize": self.cli.window_size,
            "actualViewportSize": [ctx.screen_rect().width(), ctx.screen_rect().height()],
            "pixelsPerPoint": ctx.pixels_per_point(),
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
            "menuRect": rect_json(self.diagnostic_menu_rect),
            "transportRect": rect_json(self.diagnostic_transport_rect),
            "headerRect": rect_json(self.diagnostic_header_rect),
            "toastRect": rect_json(self.diagnostic_toast_rect),
            "resizeHovered": self.resize_hovered,
            "cursorIcon": ctx.output(|output| format!("{:?}", output.cursor_icon)),
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
            self.mark_smoke_report_written();
        }
    }

    fn mark_smoke_report_written(&mut self) {
        self.smoke_report_written = true;
        self.smoke_report_update.get_or_insert(self.update_count);
    }

    fn smoke_exit_ready(&self, screenshot_succeeded: bool) -> bool {
        self.cli.quit_after_smoke
            && self.smoke_report_written
            && screenshot_succeeded
            && self
                .smoke_report_update
                .is_some_and(|written_at| self.update_count >= written_at.saturating_add(3))
    }

    fn draw_canvas_surface(&mut self, ctx: &EguiContext) {
        self.ensure_assets(ctx);
        let modal_active = self.show_youtube.is_some() || self.show_text_dialog;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let canvas_rect = ui.max_rect();
                let canvas = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
                let painter = ui.painter_at(canvas_rect);
                self.paint_grid(&painter, canvas_rect);
                self.resize_hovered = false;
                let pointer = ctx.input(|input| input.pointer.interact_pos());
                let overlay_blocks = modal_active
                    || self.context_menu_pos.is_some()
                    || pointer.is_some_and(|position| self.hovered_header && position.y <= 42.0)
                    || pointer.is_some_and(|position| {
                        self.selected
                            .and_then(|index| self.scene.elements.get(index))
                            .is_some_and(|element| {
                                let has_duration = element.media_duration.is_some()
                                    || self
                                        .presented_playback
                                        .get(&element.id)
                                        .and_then(|state| state.duration_seconds)
                                        .is_some()
                                    || self
                                        .playback_sessions
                                        .get(&element.id)
                                        .and_then(|session| session.snapshot().duration_seconds)
                                        .is_some();
                                transport_overlay_rect(ctx, &self.scene, element, has_duration)
                                    .contains(position)
                            })
                    });
                let (space_pan, ctrl_space_zoom) = ctx.input(|input| {
                    (
                        input.pointer.primary_down()
                            && input.key_down(Key::Space)
                            && !input.modifiers.ctrl,
                        input.pointer.primary_down()
                            && input.key_down(Key::Space)
                            && input.modifiers.ctrl,
                    )
                });
                let mut bring_front = None;
                for index in 0..self.scene.elements.len() {
                    let (world_x, world_y, width, height, id) = {
                        let element = &self.scene.elements[index];
                        (
                            element.x,
                            element.y,
                            element.width.unwrap_or(320.0),
                            element.height.unwrap_or(200.0),
                            element.id,
                        )
                    };
                    let scale = self.scene.current_scale.max(0.01);
                    let origin =
                        canvas_rect.min + Vec2::new(self.scene.translate.x, self.scene.translate.y);
                    let rect = Rect::from_min_size(
                        origin + Vec2::new(world_x, world_y) * scale,
                        Vec2::new(width, height) * scale,
                    );
                    let response =
                        ui.interact(rect, ui.id().with(("element", id)), Sense::click_and_drag());
                    let selected = self.selected_id() == Some(id);
                    let handle_rect = Rect::from_min_size(
                        rect.right_bottom() - Vec2::splat(12.0),
                        Vec2::splat(12.0),
                    );
                    let handle = selected.then(|| {
                        ui.interact(handle_rect, ui.id().with(("resize", id)), Sense::drag())
                    });
                    if selected
                        && handle.as_ref().is_some_and(|handle| {
                            handle.hovered() || handle.is_pointer_button_down_on()
                        })
                    {
                        self.resize_hovered = true;
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                    if !overlay_blocks && response.clicked_by(egui::PointerButton::Primary) {
                        self.selected = Some(index);
                        bring_front = Some(id);
                    }
                    if !overlay_blocks
                        && response.dragged_by(egui::PointerButton::Primary)
                        && !handle
                            .as_ref()
                            .is_some_and(|handle| handle.dragged_by(egui::PointerButton::Primary))
                        && !space_pan
                        && !ctrl_space_zoom
                    {
                        let delta = ctx.input(|input| input.pointer.delta()) / scale;
                        let element = &mut self.scene.elements[index];
                        element.x += delta.x;
                        element.y += delta.y;
                        self.selected = Some(index);
                    }
                    if !overlay_blocks
                        && self.scene.grid.enabled
                        && response.drag_stopped_by(egui::PointerButton::Primary)
                    {
                        let spacing = self.scene.grid.spacing.max(1.0);
                        let element = &mut self.scene.elements[index];
                        element.x = (element.x / spacing).round() * spacing;
                        element.y = (element.y / spacing).round() * spacing;
                    }
                    if !overlay_blocks
                        && selected
                        && handle
                            .as_ref()
                            .is_some_and(|handle| handle.dragged_by(egui::PointerButton::Primary))
                    {
                        let delta = ctx.input(|input| input.pointer.delta()) / scale;
                        let aspect = (width / height).max(0.01);
                        let element = &mut self.scene.elements[index];
                        if (delta.x / width.max(1.0)).abs() >= (delta.y / height.max(1.0)).abs() {
                            let new_width = (width + delta.x).max(10.0);
                            element.width = Some(new_width);
                            element.height = Some((new_width / aspect).max(10.0));
                        } else {
                            let new_height = (height + delta.y).max(10.0);
                            element.height = Some(new_height);
                            element.width = Some((new_height * aspect).max(10.0));
                        }
                    }
                    if !overlay_blocks
                        && selected
                        && self.scene.grid.enabled
                        && handle.as_ref().is_some_and(|handle| {
                            handle.drag_stopped_by(egui::PointerButton::Primary)
                        })
                    {
                        let spacing = self.scene.grid.spacing.max(1.0);
                        let element = &mut self.scene.elements[index];
                        let aspect = (width / height).max(0.01);
                        let new_width =
                            (element.width.unwrap_or(width) / spacing).round().max(1.0) * spacing;
                        element.width = Some(new_width);
                        element.height = Some((new_width / aspect).max(10.0));
                    }
                    self.paint_element(&painter, index, rect);
                    if selected {
                        painter.rect_stroke(
                            rect,
                            0.0,
                            Stroke::new(1.5_f32, Color32::from_rgb(45, 165, 210)),
                            egui::StrokeKind::Inside,
                        );
                        painter.rect_filled(handle_rect, 1.0, Color32::from_rgb(45, 165, 210));
                    }
                    if let Some(error) = self.playback_errors.get(&id) {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            error,
                            FontId::proportional(13.0),
                            Color32::LIGHT_RED,
                        );
                    }
                    if self.scene.elements[index].media_type == "youtube"
                        && self.scene.elements[index].display_path().is_none()
                    {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Choose a section to download",
                            FontId::proportional(15.0),
                            Color32::LIGHT_GRAY,
                        );
                    }
                }
                if let Some(id) = bring_front
                    && let Some(index) = self
                        .scene
                        .elements
                        .iter()
                        .position(|element| element.id == id)
                {
                    self.bring_to_front(index);
                }
                if !overlay_blocks
                    && canvas.clicked_by(egui::PointerButton::Primary)
                    && bring_front.is_none()
                {
                    self.selected = None;
                }
                let pointer_in_canvas = pointer.is_some_and(|point| canvas_rect.contains(point));
                if !overlay_blocks
                    && pointer_in_canvas
                    && (space_pan
                        || ctx
                            .input(|input| input.pointer.button_down(egui::PointerButton::Middle)))
                {
                    let delta = ctx.input(|input| input.pointer.delta());
                    self.scene.translate.x += delta.x;
                    self.scene.translate.y += delta.y;
                }
                if !overlay_blocks
                    && let Some(position) =
                        pointer.filter(|position| canvas_rect.contains(*position))
                {
                    let scroll = ctx.input(|input| input.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        let old = self.scene.current_scale;
                        let new = (old * (1.0 + scroll * 0.001)).clamp(0.01, 2.0);
                        let anchor = position
                            - canvas_rect.min
                            - Vec2::new(self.scene.translate.x, self.scene.translate.y);
                        let factor = 1.0 - new / old;
                        self.scene.translate.x += anchor.x * factor;
                        self.scene.translate.y += anchor.y * factor;
                        self.scene.current_scale = new;
                        ctx.request_repaint();
                    }
                    if ctrl_space_zoom {
                        let delta = ctx.input(|input| input.pointer.delta().x);
                        if delta != 0.0 {
                            let old = self.scene.current_scale;
                            let new = (old + delta / 120.0 * old / 2.0).clamp(0.01, 2.0);
                            let anchor = position
                                - canvas_rect.min
                                - Vec2::new(self.scene.translate.x, self.scene.translate.y);
                            let factor = 1.0 - new / old;
                            self.scene.translate.x += anchor.x * factor;
                            self.scene.translate.y += anchor.y * factor;
                            self.scene.current_scale = new;
                        }
                    }
                }
                if !modal_active && ctx.input(|input| !input.raw.dropped_files.is_empty()) {
                    for file in ctx.input(|input| input.raw.dropped_files.clone()) {
                        if let Some(path) = file.path {
                            self.import_path(&path);
                        }
                    }
                }
                let _ = canvas;
            });
    }

    fn paint_grid(&self, painter: &egui::Painter, rect: Rect) {
        if !self.scene.grid.enabled {
            return;
        }
        let mut step = self.scene.grid.spacing.max(1.0) * self.scene.current_scale.max(0.01);
        while step < 24.0 {
            step *= 2.0;
        }
        if step > 240.0 {
            return;
        }
        let origin = rect.min + Vec2::new(self.scene.translate.x, self.scene.translate.y);
        let first_x = rect.left() + (origin.x - rect.left()).rem_euclid(step);
        let first_y = rect.top() + (origin.y - rect.top()).rem_euclid(step);
        let color = Color32::from_gray(48);
        let mut x = first_x;
        while x < rect.right() {
            let mut y = first_y;
            while y < rect.bottom() {
                painter.circle_filled(Pos2::new(x, y), 1.15, color);
                y += step;
            }
            x += step;
        }
    }

    fn paint_element(&mut self, painter: &egui::Painter, index: usize, rect: Rect) {
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
        let is_animated = element.is_video() || self.playback_sessions.contains_key(&element.id);
        if is_animated {
            if let Some(texture) = self.playback_textures.get(&element.id) {
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
            if let Some(error) = self.playback_errors.get(&element.id) {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    error,
                    FontId::proportional(13.0),
                    Color32::LIGHT_RED,
                );
            }
            return;
        }
        let key = element.id.to_string();
        if let Some(texture) = self.image_assets.get(&key) {
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
                        DownloadEvent::Progress(progress) => dialog.progress = Some(progress),
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
        let mut open = true;
        let dialog_width = (ctx.screen_rect().width() - 24.0).clamp(220.0, 430.0);
        egui::Window::new("Prepare YouTube clip")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(dialog_width)
            .min_width(dialog_width.min(270.0))
            .max_width(dialog_width)
            .show(ctx, |ui| {
                let body_height = (ctx.screen_rect().height() - 92.0).max(112.0);
                egui::ScrollArea::vertical()
                    .max_height(body_height)
                    .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label("Inspect a link, then choose a source range.");
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
                        ui.label("Start");
                        ui.add_sized([ui.available_width(), 24.0], egui::TextEdit::singleline(&mut dialog.start_text));
                        ui.label("End");
                        ui.add_sized([ui.available_width(), 24.0], egui::TextEdit::singleline(&mut dialog.end_text));
                    });
                    ui.label("Enter seconds or HH:MM:SS.")
                        .on_hover_text("The source video is downloaded only after you click Download selected section.");
                    if let Some(job) = &dialog.job {
                        if let Some(progress) = &dialog.progress {
                            let stage = match progress.stage {
                                DownloadStage::Preparing => "Preparing",
                                DownloadStage::Fetching => "Fetching",
                                DownloadStage::Finalizing => "Finalizing",
                                DownloadStage::Verifying => "Verifying",
                                DownloadStage::Finished => "Finished",
                            };
                            if matches!(progress.stage, DownloadStage::Fetching)
                                && let (Some(elapsed), Some(target)) = (progress.elapsed_seconds, progress.target_seconds)
                                && target > 0.0
                            {
                                ui.label("Section processing");
                                ui.add(egui::ProgressBar::new((elapsed / target).clamp(0.0, 1.0) as f32).text(format!("{elapsed:.1}/{target:.1}s")));
                            } else {
                                ui.horizontal(|ui| { ui.spinner(); ui.label(stage); });
                            }
                            if let Some(bytes) = &progress.output_bytes { ui.label(format!("Output: {:.1} MB", *bytes as f64 / 1_000_000.0)); }
                            if let Some(detail) = &progress.detail { ui.label(detail); }
                        } else {
                            ui.horizontal(|ui| { ui.spinner(); ui.label("Preparing"); });
                        }
                        if ui.button("Cancel download").clicked() { job.cancel(); }
                    } else if ui.button("Download selected section").clicked() {
                        let start = parse_time(&dialog.start_text);
                        let end = parse_time(&dialog.end_text);
                        let duration = metadata.duration.unwrap_or(0.0);
                        if let (Some(start), Some(end)) = (start, end) {
                            if start >= 0.0 && start < end && end <= duration + 0.01 {
                                match youtube::begin_download(dialog.url.clone(), metadata.id.clone(), start, end, "best".into()) {
                                    Ok(job) => { dialog.job = Some(job); dialog.progress = None; dialog.error = None; }
                                    Err(error) => dialog.error = Some(error.to_string()),
                                }
                            } else { dialog.error = Some("Choose a valid range within the source duration.".into()); }
                        } else { dialog.error = Some("Enter the times as seconds or HH:MM:SS.".into()); }
                    }
                }
                if let Some(error) = &dialog.error { ui.colored_label(Color32::LIGHT_RED, error); }
                    });
            });
        if !open {
            if let Some(dialog) = self.show_youtube.as_ref() {
                if let Some(job) = &dialog.job {
                    job.cancel();
                }
                if let Some(job) = &dialog.metadata_job {
                    job.cancel();
                }
            }
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
            self.clear_item_cache(self.scene.elements[index].id);
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
    }

    fn begin_window_resize(&self, ctx: &EguiContext) {
        if self.show_youtube.is_some() || self.show_text_dialog || self.context_menu_pos.is_some() {
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
        let (native_copy, native_paste) = ctx.input(|input| {
            let copy = input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy));
            let paste = input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Paste(_)));
            (copy, paste)
        });
        let win32_paste = window_host::take_paste_request();
        if !wants_keyboard {
            if native_copy {
                self.queue_action(AppAction::CopySelected);
            }
            if native_paste || win32_paste {
                self.queue_action(AppAction::Paste);
            }
            if native_copy || native_paste {
                ctx.input_mut(|input| {
                    input
                        .events
                        .retain(|event| !matches!(event, egui::Event::Copy | egui::Event::Paste(_)))
                });
            }
        }
        let save_as = !wants_keyboard && consume_ctrl_shortcut(ctx, Key::S, true);
        if save_as {
            self.queue_action(AppAction::SaveAs);
        } else if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::S, false) {
            self.queue_action(AppAction::Save);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::L, false) {
            self.queue_action(AppAction::OpenScene);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::W, false) {
            self.queue_action(AppAction::Close);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::F, false) {
            self.queue_action(AppAction::ToggleMaximize);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::M, false) {
            self.queue_action(AppAction::Minimize);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::N, false) {
            self.queue_action(AppAction::NewScene);
        }
        if !wants_keyboard && consume_ctrl_shortcut(ctx, Key::G, false) {
            self.queue_action(AppAction::ToggleGrid);
        }
        if !wants_keyboard && ctx.input(|input| input.key_pressed(Key::Delete)) {
            self.queue_action(AppAction::DeleteSelected);
        }
        if ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.context_menu_pos = None;
            self.context_menu_item = false;
            self.context_submenu = None;
            self.context_submenu_anchor = None;
        }
        if !wants_keyboard && ctx.input(|input| input.key_pressed(Key::K)) {
            self.queue_action(AppAction::PlayPause);
        }
        if !wants_keyboard && ctx.input(|input| input.key_pressed(Key::ArrowLeft)) {
            self.queue_action(AppAction::StepAnimation(-1));
        }
        if !wants_keyboard && ctx.input(|input| input.key_pressed(Key::ArrowRight)) {
            self.queue_action(AppAction::StepAnimation(1));
        }
    }

    fn capture_result(&mut self, result: Result<()>) {
        if let Err(error) = result {
            self.status = format!("{error:#}");
        }
    }
}

impl eframe::App for AnimRefApp {
    fn update(&mut self, ctx: &EguiContext, frame: &mut eframe::Frame) {
        self.update_diagnostics(ctx);
        self.handle_shortcuts(ctx);
        let (right_pressed, right_held, right_released, local_position) = ctx.input(|input| {
            (
                input.pointer.button_pressed(egui::PointerButton::Secondary),
                input.pointer.button_down(egui::PointerButton::Secondary),
                input
                    .pointer
                    .button_released(egui::PointerButton::Secondary),
                input.pointer.interact_pos(),
            )
        });
        let start_allowed = self.show_youtube.is_none()
            && !self.show_text_dialog
            && self.context_menu_pos.is_none();
        if let Some(position) = self.window_host.update(
            frame,
            right_pressed,
            right_held,
            right_released,
            local_position,
            start_allowed,
        ) && self.show_youtube.is_none()
            && !self.show_text_dialog
        {
            self.select_under_pointer(position);
            self.context_menu_pos = Some(position);
            self.context_menu_item = self.selected_id().is_some();
            self.context_submenu = None;
        }
        self.run_actions(ctx);
        if let Some(result) =
            self.dialogs
                .advance(ctx, frame, self.update_count, self.always_on_top)
        {
            self.handle_dialog_result(result);
        }
        self.drain_remote_imports();
        self.drain_pinterest_imports();
        self.draw_canvas_surface(ctx);
        self.draw_hover_header(ctx);
        self.draw_selection_bar(ctx);
        self.draw_context_menu(ctx);
        self.draw_youtube_dialog(ctx);
        self.draw_other_windows(ctx);
        self.draw_toast(ctx);
        self.begin_window_resize(ctx);
        if self.smoke_report_written {
            self.write_smoke_report(ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

impl AnimRefApp {
    fn select_under_pointer(&mut self, pointer: Pos2) {
        let origin = ctx_origin_for(&self.scene, pointer);
        let scale = self.scene.current_scale.max(0.01);
        let mut selected = None;
        for (index, element) in self.scene.elements.iter().enumerate().rev() {
            let rect = Rect::from_min_size(
                origin + Vec2::new(element.x, element.y) * scale,
                Vec2::new(
                    element.width.unwrap_or(320.0),
                    element.height.unwrap_or(200.0),
                ) * scale,
            );
            if rect.contains(pointer) {
                selected = Some(index);
                break;
            }
        }
        self.selected = selected;
    }
}

fn ctx_origin_for(scene: &Scene, pointer: Pos2) -> Pos2 {
    let _ = pointer;
    Pos2::ZERO + Vec2::new(scene.translate.x, scene.translate.y)
}

fn consume_ctrl_shortcut(ctx: &EguiContext, key: Key, shift: bool) -> bool {
    let modifiers = if shift {
        egui::Modifiers::CTRL | egui::Modifiers::SHIFT
    } else {
        egui::Modifiers::CTRL
    };
    ctx.input_mut(|input| input.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, key)))
}

fn copy_file_atomically(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    fs::copy(source, temporary.path())
        .with_context(|| format!("Could not stage {}", source.display()))?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("Could not replace {}", destination.display()))?;
    Ok(())
}

fn write_file_atomically(destination: &Path, contents: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temporary, contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("Could not replace {}", destination.display()))?;
    Ok(())
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

fn cached_youtube_path(element: &Element) -> Option<&Path> {
    (element.media_type == "youtube")
        .then_some(element.cached_path.as_deref())
        .flatten()
        .filter(|path| path.is_file())
}

fn remote_playback_url(element: &Element) -> Option<&str> {
    (element.media_type == "video" && is_http_url(&element.path)).then_some(element.path.as_str())
}

fn ellipsized_galley(
    painter: &egui::Painter,
    text: &str,
    font_id: egui::FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let full = painter.layout_no_wrap(text.to_owned(), font_id.clone(), color);
    if full.size().x <= max_width {
        return full;
    }
    let characters: Vec<char> = text.chars().collect();
    let mut low = 0;
    let mut high = characters.len();
    let mut best = painter.layout_no_wrap("…".to_owned(), font_id.clone(), color);
    while low <= high {
        let middle = low + (high - low) / 2;
        let prefix: String = characters.iter().take(middle).collect();
        let candidate = painter.layout_no_wrap(format!("{prefix}…"), font_id.clone(), color);
        if candidate.size().x <= max_width {
            best = candidate;
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    best
}

fn transport_overlay_rect(
    ctx: &EguiContext,
    scene: &Scene,
    element: &Element,
    has_timeline: bool,
) -> Rect {
    let screen = ctx.screen_rect();
    let size = Vec2::new(
        (if has_timeline { 300.0_f32 } else { 160.0_f32 }).min((screen.width() - 16.0).max(120.0)),
        50.0,
    );
    let scale = scene.current_scale.max(0.01);
    let item = Rect::from_min_size(
        screen.min
            + Vec2::new(scene.translate.x, scene.translate.y)
            + Vec2::new(element.x, element.y) * scale,
        Vec2::new(
            element.width.unwrap_or(320.0),
            element.height.unwrap_or(200.0),
        ) * scale,
    );
    let mut x = item.center().x - size.x * 0.5;
    x = x.clamp(
        screen.left() + 8.0,
        (screen.right() - size.x - 8.0).max(screen.left() + 8.0),
    );
    let below = item.bottom() + 8.0;
    let y = if below + size.y <= screen.bottom() - 8.0 {
        below
    } else {
        (item.top() - size.y - 8.0).max(screen.top() + 8.0)
    };
    Rect::from_min_size(Pos2::new(x, y), size)
}

fn rect_json(rect: Option<Rect>) -> serde_json::Value {
    rect.map(|rect| {
        serde_json::json!({
            "left": rect.left(), "top": rect.top(), "right": rect.right(), "bottom": rect.bottom(),
            "width": rect.width(), "height": rect.height()
        })
    })
    .unwrap_or(serde_json::Value::Null)
}

fn context_menu_row(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width().min(228.0), 25.0), sense);
    let visuals = if response.is_pointer_button_down_on() {
        &ui.visuals().widgets.active
    } else if response.hovered() {
        &ui.visuals().widgets.hovered
    } else {
        &ui.visuals().widgets.inactive
    };
    if response.hovered() || response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
    }
    let color = if enabled {
        visuals.text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let galley = ellipsized_galley(
        ui.painter(),
        label,
        egui::FontId::proportional(15.0),
        color,
        (rect.width() - 48.0).max(1.0),
    );
    ui.painter()
        .with_clip_rect(rect.shrink2(Vec2::new(7.0, 0.0)))
        .galley(
            Pos2::new(rect.left() + 31.0, rect.center().y - galley.size().y * 0.5),
            galley,
            color,
        );
    response
}

fn context_menu_check_row(ui: &mut egui::Ui, label: &str, checked: bool) -> egui::Response {
    let response = context_menu_row(ui, label, true);
    if checked {
        ui.painter().text(
            Pos2::new(response.rect.left() + 14.0, response.rect.center().y),
            egui::Align2::CENTER_CENTER,
            egui_phosphor::regular::CHECK,
            phosphor_font(16.0),
            ui.visuals().text_color(),
        );
    }
    response
}

fn context_menu_submenu_row(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let response = context_menu_row(ui, label, true);
    ui.painter().text(
        Pos2::new(response.rect.right() - 15.0, response.rect.center().y),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::CARET_RIGHT,
        phosphor_font(15.0),
        if active {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        },
    );
    response
}

fn icon_button(ui: &mut egui::Ui, icon: &str, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
    if response.hovered() || response.is_pointer_button_down_on() {
        ui.painter()
            .rect_filled(rect, 5.0, ui.visuals().widgets.hovered.bg_fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        phosphor_font(16.0),
        ui.visuals().text_color(),
    );
    response.on_hover_text(tooltip)
}

fn phosphor_font(size: f32) -> FontId {
    FontId::proportional(size)
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

fn clipboard_path(text: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(text.trim().trim_matches('"'));
    (candidate.is_file()).then_some(candidate)
}

fn install_cjk_font(ctx: &EguiContext) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
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
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn is_youtube_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("youtube.com/") || lower.contains("youtu.be/"))
        && (lower.starts_with("http://") || lower.starts_with("https://"))
}

fn is_pinterest_pin_url(text: &str) -> bool {
    let Some((scheme, remainder)) = text.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let host_and_path = remainder.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = host_and_path.split_once('/').unwrap_or((host_and_path, ""));
    let host = host
        .split('@')
        .next_back()
        .unwrap_or(host)
        .split(':')
        .next()
        .unwrap_or(host)
        .to_ascii_lowercase();
    if host == "pin.it" || host == "www.pin.it" {
        return !path.is_empty();
    }
    (host == "pinterest.com"
        || host.ends_with(".pinterest.com")
        || host.strip_prefix("pinterest.").is_some())
        && path.split('/').any(|part| part.eq_ignore_ascii_case("pin"))
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
            show_youtube: None,
            show_text_dialog: false,
            text_draft: String::new(),
            image_assets: HashMap::new(),
            image_errors: HashMap::new(),
            remote_jobs: Vec::new(),
            pinterest_jobs: Vec::new(),
            remote_pending: HashSet::new(),
            scene_generation: 1,
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
            actions: VecDeque::new(),
            dialogs: DialogCoordinator::default(),
            toast: None,
            context_menu_pos: None,
            context_menu_item: false,
            context_submenu: None,
            context_submenu_anchor: None,
            diagnostic_menu_rect: None,
            diagnostic_transport_rect: None,
            diagnostic_header_rect: None,
            diagnostic_toast_rect: None,
            resize_hovered: false,
            hovered_header: false,
            last_presented_position: HashMap::new(),
            animation_paused: HashMap::new(),
            texture_paths: HashMap::new(),
            playback_sessions: HashMap::new(),
            playback_textures: HashMap::new(),
            playback_errors: HashMap::new(),
            expected_playback_epoch: HashMap::new(),
            presented_playback: HashMap::new(),
            pending_seeks: HashMap::new(),
            playback_scenario_pending: false,
            observed_status: String::new(),
            window_host: window_host::WindowHost::default(),
        }
    }

    fn canvas_frame(context: &EguiContext, app: &mut AnimRefApp, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = context.run(input, |ctx| app.draw_canvas_surface(ctx));
    }

    fn selection_bar_frame(context: &EguiContext, app: &mut AnimRefApp, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = context.run(input, |ctx| app.draw_selection_bar(ctx));
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
    fn parses_minimum_diagnostic_window_size() {
        let options = CliOptions::parse(["--window-size", "320x220"].map(std::ffi::OsString::from));
        assert_eq!(options.window_size, Some([320.0, 220.0]));
    }

    #[test]
    fn youtube_playback_uses_an_existing_cache_and_never_streams_the_page_url() {
        let mut youtube = Element::new(
            "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
            "youtube",
            0.0,
            0.0,
            640.0,
            360.0,
        );
        assert!(cached_youtube_path(&youtube).is_none());
        assert!(remote_playback_url(&youtube).is_none());

        let directory = tempfile::tempdir().expect("create cache fixture directory");
        let cache = directory.path().join("section.mp4");
        fs::write(&cache, b"cached youtube section").expect("write cache fixture");
        youtube.cached_path = Some(cache.clone());
        assert_eq!(cached_youtube_path(&youtube), Some(cache.as_path()));
        assert!(remote_playback_url(&youtube).is_none());

        let remote_video = Element::new(
            "https://cdn.example.test/video.mp4".into(),
            "video",
            0.0,
            0.0,
            640.0,
            360.0,
        );
        assert_eq!(
            remote_playback_url(&remote_video),
            Some(remote_video.path.as_str())
        );
    }

    #[test]
    fn transport_does_not_seek_from_playback_position_rounding_without_input() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        let video = Element::new("fixture.mp4".into(), "video", 0.0, 0.0, 320.0, 180.0);
        let id = video.id;
        app.scene.elements.push(video);
        app.selected = Some(0);
        app.presented_playback.insert(
            id,
            media::PlaybackSnapshot {
                paused: false,
                position_seconds: 0.066_666_7,
                duration_seconds: Some(2.0),
                frame_index: Some(1),
                epoch: 1,
            },
        );

        let context = EguiContext::default();
        for _ in 0..6 {
            selection_bar_frame(&context, &mut app, Vec::new());
            assert!(
                app.actions.is_empty(),
                "playhead display rounding queued a seek"
            );
            assert!(
                app.pending_seeks.is_empty(),
                "playhead display rounding staged a seek"
            );
        }
    }

    #[test]
    fn resize_handle_hover_uses_diagonal_resize_cursor() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        app.scene.elements.push(Element::new(
            "Resize cursor fixture".into(),
            "text",
            100.0,
            100.0,
            200.0,
            100.0,
        ));
        app.selected = Some(0);
        let context = EguiContext::default();
        let frame = |context: &EguiContext, app: &mut AnimRefApp, events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| app.draw_canvas_surface(ctx),
            )
        };
        let _ = frame(&context, &mut app, Vec::new());
        let output = frame(
            &context,
            &mut app,
            vec![egui::Event::PointerMoved(Pos2::new(294.0, 194.0))],
        );
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::ResizeNwSe
        );
    }

    #[test]
    fn smoke_exit_timer_stays_anchored_to_the_first_report() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        app.cli.quit_after_smoke = true;
        app.update_count = 10;
        app.mark_smoke_report_written();
        assert_eq!(app.smoke_report_update, Some(10));
        assert!(!app.smoke_exit_ready(true));

        app.update_count = 12;
        app.mark_smoke_report_written();
        assert_eq!(app.smoke_report_update, Some(10));
        assert!(!app.smoke_exit_ready(true));
        assert!(!app.smoke_exit_ready(false));

        app.update_count = 13;
        app.mark_smoke_report_written();
        assert_eq!(app.smoke_report_update, Some(10));
        assert!(app.smoke_exit_ready(true));
    }

    #[test]
    fn native_copy_event_routes_to_selected_item_action() {
        let mut app = app_for_shortcut_test(PathBuf::from("target/acceptance/unused.purgif"));
        let mut element = Element::new("clipboard text".into(), "text", 0.0, 0.0, 100.0, 30.0);
        element.id = 41;
        app.scene.elements.push(element);
        app.selected = Some(0);
        let context = EguiContext::default();
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Copy],
                ..Default::default()
            },
            |ctx| app.handle_shortcuts(ctx),
        );
        assert!(matches!(app.actions.front(), Some(AppAction::CopySelected)));
        assert!(context.input(|input| input.events.is_empty()));
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
    fn canvas_primary_drag_snaps_position_on_release_in_world_coordinates() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        app.scene.current_scale = 2.0;
        app.scene.grid = model::GridSettings {
            enabled: true,
            spacing: 32.0,
        };
        let item = Element::new("snap me".to_owned(), "text", 50.0, 60.0, 100.0, 80.0);
        let id = item.id;
        app.scene.elements.push(item);
        let context = EguiContext::default();
        let start = Pos2::new(110.0, 120.0);
        let end = Pos2::new(140.0, 90.0);
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
        canvas_frame(&context, &mut app, vec![egui::Event::PointerMoved(end)]);
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
        assert_eq!((moved.x, moved.y), (64.0, 32.0));
    }

    #[test]
    fn canvas_secondary_drag_does_not_move_selected_item() {
        let mut app = app_for_shortcut_test(PathBuf::new());
        let item = Element::new("stay put".to_owned(), "text", 50.0, 60.0, 100.0, 80.0);
        app.scene.elements.push(item);
        app.selected = Some(0);
        let context = EguiContext::default();
        let start = Pos2::new(70.0, 80.0);
        let end = Pos2::new(130.0, 120.0);
        canvas_frame(&context, &mut app, Vec::new());
        canvas_frame(
            &context,
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        canvas_frame(&context, &mut app, vec![egui::Event::PointerMoved(end)]);
        canvas_frame(
            &context,
            &mut app,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        assert_eq!(
            (app.scene.elements[0].x, app.scene.elements[0].y),
            (50.0, 60.0)
        );
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
        let _ = context.run(input, |ctx| {
            app.handle_shortcuts(ctx);
            app.run_actions(ctx);
        });

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
