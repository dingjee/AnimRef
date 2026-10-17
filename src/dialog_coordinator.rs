use eframe::egui::{Context, ViewportCommand, WindowLevel};
use std::{collections::VecDeque, path::PathBuf};

#[derive(Debug)]
pub enum DialogRequest {
    OpenScene,
    SaveScene,
    ImportMedia,
    ExportFile {
        source: PathBuf,
        suggested_name: String,
    },
    SaveText {
        contents: String,
        suggested_name: String,
    },
}

#[derive(Debug)]
pub enum DialogResult {
    OpenScene(Option<PathBuf>),
    SaveScene(Option<PathBuf>),
    ImportMedia(Option<Vec<PathBuf>>),
    ExportFile {
        source: PathBuf,
        destination: Option<PathBuf>,
    },
    SaveText {
        contents: String,
        destination: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub enum DialogOwner {
    #[default]
    MainWindow,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Lowering {
        request: DialogRequest,
        restore_topmost: bool,
        requested_at_update: u64,
        _owner: DialogOwner,
    },
}

#[derive(Default)]
pub struct DialogCoordinator {
    pending: VecDeque<DialogRequest>,
    phase: Phase,
}

impl DialogCoordinator {
    pub fn enqueue(&mut self, request: DialogRequest) {
        self.pending.push_back(request);
    }

    pub fn advance(
        &mut self,
        context: &Context,
        frame: &eframe::Frame,
        update_count: u64,
        restore_topmost: bool,
    ) -> Option<DialogResult> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => {
                let request = self.pending.pop_front()?;
                context.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::Normal));
                context.request_repaint();
                self.phase = Phase::Lowering {
                    request,
                    restore_topmost,
                    requested_at_update: update_count,
                    _owner: DialogOwner::MainWindow,
                };
                None
            }
            Phase::Lowering {
                request,
                restore_topmost: should_restore,
                requested_at_update,
                _owner: _,
            } if update_count > requested_at_update => {
                let result = run_dialog(request, frame);
                context.send_viewport_cmd(ViewportCommand::WindowLevel(if should_restore {
                    WindowLevel::AlwaysOnTop
                } else {
                    WindowLevel::Normal
                }));
                if !self.pending.is_empty() {
                    context.request_repaint();
                }
                Some(result)
            }
            phase @ Phase::Lowering { .. } => {
                self.phase = phase;
                None
            }
        }
    }
}

fn run_dialog(request: DialogRequest, parent: &eframe::Frame) -> DialogResult {
    match request {
        DialogRequest::OpenScene => DialogResult::OpenScene(
            rfd::FileDialog::new()
                .set_parent(parent)
                .add_filter("AnimRef scene", &["purgif"])
                .pick_file(),
        ),
        DialogRequest::SaveScene => DialogResult::SaveScene(
            rfd::FileDialog::new()
                .set_parent(parent)
                .add_filter("AnimRef scene", &["purgif"])
                .set_file_name("scene.purgif")
                .save_file(),
        ),
        DialogRequest::ImportMedia => DialogResult::ImportMedia(
            rfd::FileDialog::new()
                .set_parent(parent)
                .add_filter(
                    "Images and video",
                    &[
                        "png", "jpg", "jpeg", "gif", "webp", "bmp", "mp4", "mov", "m4v", "mkv",
                        "webm", "avi",
                    ],
                )
                .pick_files(),
        ),
        DialogRequest::ExportFile {
            source,
            suggested_name,
        } => DialogResult::ExportFile {
            destination: rfd::FileDialog::new()
                .set_parent(parent)
                .set_file_name(suggested_name)
                .save_file(),
            source,
        },
        DialogRequest::SaveText {
            contents,
            suggested_name,
        } => DialogResult::SaveText {
            contents,
            destination: rfd::FileDialog::new()
                .set_parent(parent)
                .set_file_name(suggested_name)
                .save_file(),
        },
    }
}
