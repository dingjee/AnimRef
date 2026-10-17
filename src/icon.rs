use eframe::egui::IconData;

pub fn viewport_icon() -> std::sync::Arc<IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/animref.png"))
        .expect("embedded AnimRef icon must decode")
        .into_rgba8();
    let (width, height) = image.dimensions();
    std::sync::Arc::new(IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}
