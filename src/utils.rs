use std::{f32, sync::Arc};

use eframe::{
    egui::{self, Color32, Context, FontData, FontDefinitions, FontFamily},
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};
use itertools::Itertools;

const JETBRAINS_MONO_NF: &'static str = "JetbrainsMonoNG";

pub fn add_font(ctx: &Context) {
    ctx.add_font(FontInsert::new(
        JETBRAINS_MONO_NF,
        FontData::from_static(include_bytes!("../fonts/JetBrainsMonoNerdFont-Regular.ttf")),
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Highest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
}

pub fn replace_font(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        JETBRAINS_MONO_NF.to_string(),
        Arc::new(FontData::from_static(include_bytes!(
            "../fonts/JetBrainsMonoNerdFont-Regular.ttf"
        ))),
    );

    // Put my font first (highest priority) for proportional text:
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, JETBRAINS_MONO_NF.to_string());

    // Put my font as last fallback for monospace:
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push(JETBRAINS_MONO_NF.to_string());

    ctx.set_fonts(fonts);
}

pub fn get_char_size(ctx: &Context) -> (f32, f32) {
    let font_id = ctx.style().text_styles[&egui::TextStyle::Monospace].clone();
    let (width, height) = ctx.fonts(|fonts| {
        let layout = fonts.layout("@".to_string(), font_id, Color32::default(), f32::INFINITY);
        (layout.rect.width(), layout.rect.height())
    });

    (width, height)
}

pub fn character_to_screen_pos(
    char_pos: &(usize, usize),
    char_size: &(f32, f32),
    content: &[u8],
) -> (f32, f32) {
    let content_by_lines = content.split(|char| *char == b'\n').collect_vec();
    let num_lines = content_by_lines.len();
    let x_offset = char_pos.0 as f32 * char_size.0;
    let y_offset = (char_pos.1 as i64 - num_lines as i64) as f32 * char_size.1;

    (x_offset, y_offset)
}
