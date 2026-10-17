//! Styled canvas text painting and editing controls.
//!
//! The canvas owner starts editing on a double click and calls `draw_editor`
//! while the element is active. Its returned rectangles block canvas gestures.

use crate::model::{Element, TextAlignment, TextStyle};
use egui::{
    self, Align, Color32, FontFamily, FontId, Id, Layout, Painter, Pos2, Rect, Stroke, TextEdit,
    Vec2,
};

const TOOLBAR_HEIGHT: f32 = 38.0;
const TOOLBAR_WIDTH: f32 = 292.0;
const TEXT_PADDING: f32 = 4.0;

#[derive(Debug, Clone, Default)]
pub struct TextEditorState {
    pub editing_id: Option<u64>,
    original_text: Option<String>,
    focus_request: bool,
}

impl TextEditorState {
    pub fn begin(&mut self, element: &mut Element) {
        let legacy_height = element.height;
        element
            .text_style
            .get_or_insert_with(|| {
                let mut style = TextStyle::default();
                if let Some(height) =
                    legacy_height.filter(|height| height.is_finite() && *height > 0.0)
                {
                    // The legacy painter starts its fit search at box-height / 2.2.
                    // Use that as a close editable size when migrating the item.
                    style.font_size = (height / 2.2).clamp(1.0, 72.0);
                }
                style
            })
            .normalize();
        self.editing_id = Some(element.id);
        self.original_text = Some(element.path.clone());
        self.focus_request = true;
    }

    pub fn finish(&mut self) {
        self.editing_id = None;
        self.original_text = None;
        self.focus_request = false;
    }

    pub fn cancel(&mut self, element: &mut Element) {
        if self.editing_id == Some(element.id)
            && let Some(original) = self.original_text.take()
        {
            element.path = original;
        }
        self.editing_id = None;
        self.original_text = None;
        self.focus_request = false;
    }

    pub fn clear_if_not_selected(&mut self, selected_id: Option<u64>) {
        if self.editing_id.is_some_and(|id| Some(id) != selected_id) {
            self.finish();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EditorOutput {
    pub changed: bool,
    pub editor_has_focus: bool,
    pub editor_text_bounds: Option<Rect>,
    pub finish_request: bool,
    /// Retained for callers compiled against the earlier API. Escape commits
    /// the current draft, so this remains false.
    pub cancel_request: bool,
    pub blocked_rects: Vec<Rect>,
}

/// Paints styled text using the same layout job used by the editor.
pub fn paint_text(painter: &Painter, element: &Element, rect: Rect, zoom: f32) -> Rect {
    let Some(style) = element.text_style else {
        return Rect::NOTHING;
    };
    let style = normalized_style(style);
    let job = text_layout_job(
        &element.path,
        style,
        style.font_size * zoom.max(0.01),
        (rect.width() - TEXT_PADDING * 2.0).max(1.0_f32),
    );
    let galley = painter.layout_job(job);
    let pos = galley_anchor(rect, style.alignment);
    let painted_bounds = galley.mesh_bounds.translate(pos.to_vec2());
    painter
        .with_clip_rect(rect)
        .galley(pos, galley, Color32::WHITE);
    painted_bounds
}

fn galley_anchor(rect: Rect, text_alignment: TextAlignment) -> Pos2 {
    let x = match text_alignment {
        TextAlignment::Left => rect.left() + TEXT_PADDING,
        TextAlignment::Center => rect.center().x,
        TextAlignment::Right => rect.right() - TEXT_PADDING,
    };
    Pos2::new(x, rect.top() + TEXT_PADDING)
}

/// Adds text-only font families without changing the application's existing
/// regular, monospace, or Phosphor font chains.
pub fn install_text_fonts(fonts: &mut egui::FontDefinitions) {
    let candidates = [
        ("animref-text-regular", r"C:\Windows\Fonts\msyh.ttc"),
        ("animref-text-bold", r"C:\Windows\Fonts\msyhbd.ttc"),
    ];
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                name.into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
        }
    }
    let regular = fonts.font_data.contains_key("animref-text-regular");
    let bold = fonts.font_data.contains_key("animref-text-bold");
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let regular_entries = if regular {
        [vec!["animref-text-regular".into()], proportional.clone()].concat()
    } else {
        proportional.clone()
    };
    let bold_entries = if bold {
        [vec!["animref-text-bold".into()], regular_entries.clone()].concat()
    } else {
        regular_entries.clone()
    };
    fonts.families.insert(
        FontFamily::Name("animref-text-regular".into()),
        regular_entries,
    );
    fonts
        .families
        .insert(FontFamily::Name("animref-text-bold".into()), bold_entries);
}

/// Draw the editor over an active canvas item and a compact formatting bar
/// beneath it. The canvas owner commits/cancels state from the returned flags.
pub fn draw_editor(
    ctx: &egui::Context,
    state: &mut TextEditorState,
    element: &mut Element,
    rect: Rect,
    canvas_rect: Rect,
    zoom: f32,
) -> EditorOutput {
    let Some(id) = state.editing_id else {
        return EditorOutput::default();
    };
    if id != element.id {
        return EditorOutput::default();
    }
    let style = element.text_style.get_or_insert_with(TextStyle::default);
    style.normalize();
    let zoom = zoom.max(0.01);
    let initial_style = *style;
    let initial_content_size = ctx.fonts(|fonts| {
        fonts
            .layout_job(text_layout_job(
                &element.path,
                initial_style,
                initial_style.font_size * zoom,
                (rect.width() - TEXT_PADDING * 2.0).max(1.0_f32),
            ))
            .size()
    });
    let minimum_height = element.height.unwrap_or(rect.height() / zoom);
    let mut content_height =
        minimum_height.max((initial_content_size.y + TEXT_PADDING * 2.0) / zoom);
    if content_height > minimum_height {
        element.height = Some(content_height);
    }
    let mut text_rect = Rect::from_min_size(
        rect.min,
        Vec2::new(rect.width(), (content_height * zoom).max(rect.height())),
    )
    .intersect(canvas_rect);
    let initial_toolbar_rect = toolbar_rect(text_rect, canvas_rect);
    let mut output = EditorOutput {
        blocked_rects: vec![text_rect, initial_toolbar_rect],
        ..Default::default()
    };

    let editor_id = Id::new(("text-item-input", id));
    let editor_style = *style;
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        ui.fonts(|fonts| {
            let mut galley = fonts.layout_job(text_layout_job(
                text.as_str(),
                editor_style,
                editor_style.font_size * zoom,
                wrap_width.max(1.0_f32),
            ));
            normalize_galley_origin(&mut galley);
            galley
        })
    };
    let text_area = egui::Area::new(Id::new(("text-item-editor", id)))
        .order(egui::Order::Foreground)
        .fixed_pos(text_rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(text_rect.size());
            ui.set_max_size(text_rect.size());
            TextEdit::multiline(&mut element.path)
                .id(editor_id)
                .font(font_id(
                    editor_style,
                    (editor_style.font_size * zoom).max(1.0),
                ))
                .horizontal_align(alignment(editor_style.alignment))
                .layouter(&mut layouter)
                .frame(false)
                .desired_width(text_rect.width())
                .desired_rows(1)
                .min_size(text_rect.size())
                .margin(Vec2::splat(TEXT_PADDING))
                .show(ui)
        });
    output.changed |= text_area.inner.response.changed();
    output.editor_has_focus = text_area.inner.response.has_focus();
    output.editor_text_bounds = Some(
        text_area
            .inner
            .galley
            .mesh_bounds
            .translate((text_area.inner.galley_pos + text_rect.min.to_vec2()).to_vec2()),
    );
    if state.focus_request {
        // Request focus after the Area has completed its first sizing pass and
        // the TextEdit response has registered this id with egui's focus system.
        text_area.inner.response.request_focus();
        // Area's first frame is a hidden sizing pass. Keep the request pending
        // until the real widget reports focus, then leave toolbar and canvas
        // keyboard handling alone.
        state.focus_request = !output.editor_has_focus;
    }

    let current_style = *style;
    let content_size = ctx.fonts(|fonts| {
        fonts
            .layout_job(text_layout_job(
                &element.path,
                current_style,
                current_style.font_size * zoom,
                (text_rect.width() - TEXT_PADDING * 2.0).max(1.0_f32),
            ))
            .size()
    });
    content_height = minimum_height.max((content_size.y + TEXT_PADDING * 2.0) / zoom);
    if element.height.is_none_or(|height| content_height > height) {
        element.height = Some(content_height);
        output.changed = true;
        text_rect = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width(), (content_height * zoom).max(rect.height())),
        )
        .intersect(canvas_rect);
        output.blocked_rects[0] = text_rect;
    }

    let mut style_changed = false;
    let toolbar_rect = toolbar_rect(text_rect, canvas_rect);
    let toolbar = egui::Area::new(Id::new(("text-item-toolbar", id)))
        .order(egui::Order::Foreground)
        .fixed_pos(toolbar_rect.min)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(37, 40, 46))
                .stroke(Stroke::new(1.0_f32, Color32::from_gray(85)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(5, 3))
                .show(ui, |ui| {
                    let content_size = Vec2::new(TOOLBAR_WIDTH - 12.0, TOOLBAR_HEIGHT - 8.0);
                    ui.set_min_size(content_size);
                    ui.set_max_size(content_size);
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        for (alignment_value, label) in [
                            (TextAlignment::Left, egui_phosphor::regular::TEXT_ALIGN_LEFT),
                            (
                                TextAlignment::Center,
                                egui_phosphor::regular::TEXT_ALIGN_CENTER,
                            ),
                            (
                                TextAlignment::Right,
                                egui_phosphor::regular::TEXT_ALIGN_RIGHT,
                            ),
                        ] {
                            if toolbar_button(ui, label, style.alignment == alignment_value)
                                .clicked()
                            {
                                style.alignment = alignment_value;
                                style_changed = true;
                            }
                        }
                        ui.separator();
                        if toolbar_button(ui, egui_phosphor::regular::MINUS, false).clicked() {
                            style.step_font_size(-1);
                            style_changed = true;
                        }
                        ui.label(format!("{}", style.font_size as u32));
                        if toolbar_button(ui, egui_phosphor::regular::PLUS, false).clicked() {
                            style.step_font_size(1);
                            style_changed = true;
                        }
                        ui.separator();
                        if toolbar_button(ui, egui_phosphor::regular::TEXT_B, style.bold).clicked()
                        {
                            style.bold = !style.bold;
                            style_changed = true;
                        }
                        if toolbar_button(ui, egui_phosphor::regular::TEXT_ITALIC, style.italic)
                            .clicked()
                        {
                            style.italic = !style.italic;
                            style_changed = true;
                        }
                    });
                });
        });
    // Area's response rectangle size is actual, while its origin is the fixed
    // canvas-space position requested above. Rebuild the global rect so the
    // blocker matches the toolbar position even during Area's first sizing pass.
    output.blocked_rects[1] = Rect::from_min_size(toolbar_rect.min, toolbar.response.rect.size());
    output.changed |= style_changed;
    if style_changed {
        state.focus_request = true;
    }

    let input = ctx.input(|input| {
        let escape = input.key_pressed(egui::Key::Escape);
        let outside_click = input.pointer.button_pressed(egui::PointerButton::Primary)
            && input.pointer.interact_pos().is_some_and(|pos| {
                !text_rect.contains(pos) && !output.blocked_rects[1].contains(pos)
            });
        (escape, outside_click)
    });
    output.cancel_request = false;
    output.finish_request = input.0 || input.1;
    output
}

fn toolbar_rect(item_rect: Rect, canvas_rect: Rect) -> Rect {
    let width = TOOLBAR_WIDTH.min(canvas_rect.width().max(1.0));
    let x = (item_rect.center().x - width * 0.5).clamp(
        canvas_rect.left(),
        (canvas_rect.right() - width).max(canvas_rect.left()),
    );
    let below = item_rect.bottom() + 8.0;
    let y = if below + TOOLBAR_HEIGHT <= canvas_rect.bottom() {
        below
    } else {
        item_rect.top() - TOOLBAR_HEIGHT - 8.0
    }
    .clamp(
        canvas_rect.top(),
        (canvas_rect.bottom() - TOOLBAR_HEIGHT).max(canvas_rect.top()),
    );
    Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, TOOLBAR_HEIGHT))
}

fn toolbar_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let fill = if selected {
        Color32::from_rgb(42, 115, 147)
    } else {
        Color32::TRANSPARENT
    };
    ui.add_sized(
        [28.0, 28.0],
        egui::Button::new(label).fill(fill).frame(false),
    )
}

fn normalized_style(mut style: TextStyle) -> TextStyle {
    style.normalize();
    style
}

fn font_id(style: TextStyle, size: f32) -> FontId {
    let family = if style.bold {
        FontFamily::Name("animref-text-bold".into())
    } else {
        FontFamily::Name("animref-text-regular".into())
    };
    FontId::new(size.max(1.0), family)
}

/// TextEdit assumes the galley origin is its top-left when it aligns the
/// galley in its response rect. LayoutJob's centered/right galleys instead use
/// an origin at their center/right edge. Translate every positioned row and
/// visual bound together so the TextEdit builder and static painter place the
/// same glyphs at the same anchor.
fn normalize_galley_origin(galley: &mut std::sync::Arc<egui::Galley>) {
    let galley = std::sync::Arc::make_mut(galley);
    let shift = -galley.rect.left();
    if shift.abs() < f32::EPSILON {
        return;
    }
    let offset = Vec2::new(shift, 0.0);
    galley.rect = galley.rect.translate(offset);
    galley.mesh_bounds = galley.mesh_bounds.translate(offset);
    for row in &mut galley.rows {
        row.pos += offset;
    }
}

fn text_layout_job(
    text: &str,
    style: TextStyle,
    size: f32,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        font_id(style, size),
        Color32::WHITE,
        wrap_width.max(1.0_f32),
    );
    job.halign = alignment(style.alignment);
    job.wrap.max_rows = usize::MAX;
    if let Some(section) = job.sections.first_mut() {
        section.format.italics = style.italic;
    }
    job
}

fn alignment(value: TextAlignment) -> Align {
    match value {
        TextAlignment::Left => Align::Min,
        TextAlignment::Center => Align::Center,
        TextAlignment::Right => Align::Max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Scene, TextStyle};

    #[test]
    fn style_round_trips_and_legacy_text_stays_unstyled() {
        let legacy = Scene::from_json(r#"{"elements":[{"type":"text","path":"old"}]}"#).unwrap();
        assert!(legacy.elements[0].text_style.is_none());
        let mut scene = Scene::default();
        let mut text = Element::new("styled".into(), "text", 0.0, 0.0, 200.0, 60.0);
        text.text_style = Some(TextStyle {
            font_size: 34.0,
            alignment: TextAlignment::Center,
            bold: true,
            italic: true,
        });
        scene.elements.push(text);
        let reread = Scene::from_json(&scene.to_json().unwrap()).unwrap();
        assert_eq!(reread.elements[0].text_style, scene.elements[0].text_style);
    }

    #[test]
    fn fibonacci_steps_cover_arbitrary_sizes_and_boundaries() {
        let mut style = TextStyle {
            font_size: 21.0,
            ..Default::default()
        };
        style.step_font_size(1);
        assert_eq!(style.font_size, 34.0);
        style.font_size = 22.0;
        style.step_font_size(-1);
        assert_eq!(style.font_size, 21.0);
        style.font_size = 610.0;
        style.step_font_size(1);
        assert_eq!(style.font_size, 610.0);
        style.font_size = 0.0;
        style.normalize();
        assert_eq!(style.font_size, 21.0);
    }

    #[test]
    fn repairs_non_finite_and_out_of_range_persisted_sizes() {
        let mut style = TextStyle {
            font_size: f32::INFINITY,
            ..Default::default()
        };
        style.normalize();
        assert_eq!(style.font_size, 21.0);
        style.font_size = 900.0;
        style.normalize();
        assert_eq!(style.font_size, 610.0);
    }

    #[test]
    fn shared_layout_job_keeps_alignment_wrap_and_synthetic_italics() {
        let style = TextStyle {
            alignment: TextAlignment::Center,
            italic: true,
            ..Default::default()
        };
        let job = text_layout_job("中文\nLatin", style, 21.0, 120.0);
        assert_eq!(job.halign, Align::Center);
        assert_eq!(job.wrap.max_width, 120.0);
        assert!(job.sections[0].format.italics);
        assert_eq!(job.text, "中文\nLatin");
    }

    #[test]
    fn centered_and_right_aligned_glyph_bounds_stay_inside_item_rect() {
        let mut definitions = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut definitions, egui_phosphor::Variant::Regular);
        install_text_fonts(&mut definitions);
        let context = egui::Context::default();
        context.set_fonts(definitions);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            },
            |_| {},
        );
        let item_rect = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(260.0, 160.0));
        for text_alignment in [
            TextAlignment::Left,
            TextAlignment::Center,
            TextAlignment::Right,
        ] {
            let style = TextStyle {
                alignment: text_alignment,
                ..Default::default()
            };
            let mut element = Element::new(
                "中文一行\ncentered Latin".into(),
                "text",
                0.0,
                0.0,
                item_rect.width(),
                item_rect.height(),
            );
            element.text_style = Some(style);
            let mut state = TextEditorState::default();
            state.begin(&mut element);
            let mut editor_bounds = None;
            let mut paint_bounds = None;
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(canvas),
                    ..Default::default()
                },
                |ctx| {
                    let output = draw_editor(ctx, &mut state, &mut element, item_rect, canvas, 1.0);
                    editor_bounds = output.editor_text_bounds;
                    paint_bounds = Some(paint_text(
                        &ctx.layer_painter(egui::LayerId::background()),
                        &element,
                        item_rect,
                        1.0,
                    ));
                },
            );
            let editor_bounds = editor_bounds.unwrap();
            let bounds = paint_bounds.unwrap();
            assert!(
                // Egui snaps row starts to physical pixel boundaries; editor
                // and static layout can differ by a fraction of a point.
                (editor_bounds.min.x - bounds.min.x).abs() < 0.5,
                "{text_alignment:?}: editor {editor_bounds:?}, paint {bounds:?}, item {item_rect:?}"
            );
            assert!((editor_bounds.max.x - bounds.max.x).abs() < 0.5);
            assert!((editor_bounds.min.y - bounds.min.y).abs() < 0.5);
            assert!(
                item_rect.contains_rect(editor_bounds),
                "{text_alignment:?}: {editor_bounds:?}"
            );
            assert!(
                item_rect.contains_rect(bounds),
                "{text_alignment:?}: {bounds:?}"
            );
        }
    }

    #[test]
    fn toolbar_geometry_stays_centered_inside_a_320_point_canvas() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0));
        let item = Rect::from_min_size(Pos2::new(80.0, 25.0), Vec2::new(160.0, 50.0));
        let toolbar = toolbar_rect(item, canvas);
        assert_eq!(toolbar.width(), TOOLBAR_WIDTH);
        assert_eq!(toolbar.center().x, item.center().x);
        assert!(canvas.contains(toolbar.min));
        assert!(canvas.contains(toolbar.max));
    }

    #[test]
    fn editor_draft_survives_escape_and_toolbar_stays_in_reported_block_area() {
        let mut font_definitions = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut font_definitions, egui_phosphor::Variant::Regular);
        install_text_fonts(&mut font_definitions);
        let context = egui::Context::default();
        context.set_fonts(font_definitions);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 400.0));
        let item_rect = Rect::from_min_size(Pos2::new(100.0, 80.0), Vec2::new(180.0, 50.0));
        let mut element = Element::new("before".into(), "text", 0.0, 0.0, 180.0, 50.0);
        element.text_style = Some(TextStyle::default());
        let mut state = TextEditorState::default();
        state.begin(&mut element);
        element.path = "after\n第二行".into();
        let mut result = None;
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let _ = context.run(input, |ctx| {
            result = Some(draw_editor(
                ctx,
                &mut state,
                &mut element,
                item_rect,
                canvas,
                1.0,
            ));
        });
        let output = result.unwrap();
        assert!(element.height.unwrap() > 50.0);
        assert!((output.blocked_rects[1].width() - TOOLBAR_WIDTH).abs() < 0.1);
        assert_eq!(output.blocked_rects[1].center().x, item_rect.center().x);
        assert!(canvas.contains(output.blocked_rects[1].min));
        assert_eq!(element.path, "after\n第二行");
        assert!(!output.cancel_request);

        let mut escaped = None;
        let escape_input = egui::RawInput {
            screen_rect: Some(canvas),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            ..Default::default()
        };
        let _ = context.run(escape_input, |ctx| {
            escaped = Some(draw_editor(
                ctx,
                &mut state,
                &mut element,
                item_rect,
                canvas,
                1.0,
            ));
        });
        assert!(escaped.unwrap().finish_request);
        assert_eq!(element.path, "after\n第二行");
    }

    #[test]
    fn beginning_editor_focuses_left_aligned_input_after_area_sizing_pass() {
        let mut font_definitions = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut font_definitions, egui_phosphor::Variant::Regular);
        install_text_fonts(&mut font_definitions);
        let context = egui::Context::default();
        context.set_fonts(font_definitions);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 400.0));
        let item_rect = Rect::from_min_size(Pos2::new(100.0, 80.0), Vec2::new(180.0, 60.0));
        let mut element = Element::new("seed".into(), "text", 0.0, 0.0, 180.0, 60.0);
        element.text_style = Some(TextStyle::default());
        let mut state = TextEditorState::default();
        state.begin(&mut element);

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            },
            |ctx| {
                let _ = draw_editor(ctx, &mut state, &mut element, item_rect, canvas, 1.0);
            },
        );

        let mut typed_frame = None;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(canvas),
                events: vec![egui::Event::Text(" typed".into())],
                ..Default::default()
            },
            |ctx| {
                typed_frame = Some(draw_editor(
                    ctx,
                    &mut state,
                    &mut element,
                    item_rect,
                    canvas,
                    1.0,
                ));
            },
        );
        assert!(typed_frame.unwrap().editor_has_focus);
        assert_eq!(element.path, "seed typed");
        assert!(!state.focus_request);
    }
}
