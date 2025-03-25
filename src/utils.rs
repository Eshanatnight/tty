use std::sync::Arc;

use eframe::{
    egui::{Context, FontData, FontDefinitions, FontFamily},
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};

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
