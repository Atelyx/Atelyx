//! 笔记面板的帧声明：工具条 + 三档内容面 + 反链区。

use atelyx_plugin_protocol::paint::ColorToken;
use atelyx_plugin_sdk::frame_ui::{Edges, TextRole, Ui};

use crate::model::{Mode, NoteModel, READ_SCROLL, SOURCE, SOURCE_SCROLL};

/// 预览编辑的块 id；双击回发同一个 id。
pub fn block_id(index: usize) -> String {
    format!("note:block:{index}")
}

/// 空笔记（正文无块）时的编辑面 id。
pub const EMPTY_EDIT: &str = "note:empty";

pub fn frame(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    if !model.is_open() {
        ui.center_column(8.0, |ui| {
            ui.icon_named("file-text", 28.0, ColorToken::TextMuted);
            ui.text("打开笔记", TextRole::Label);
            match &model.open_error {
                Some(error) => {
                    ui.text(short(error, "打开失败"), TextRole::Caption);
                }
                None => {
                    ui.text("在文件面板单击一个 .md 笔记开始编辑。", TextRole::Caption);
                }
            }
        });
        return;
    }
    ui.fill_column(0.0, |ui| {
        toolbar(ui, model);
        ui.rule();
        match model.mode {
            Mode::Read => read_view(ui, model),
            Mode::Edit => edit_view(ui, model),
            Mode::Source => source_view(ui, model),
        }
        if !model.backlinks.is_empty() {
            ui.rule();
            backlinks(ui, model);
        }
    });
}

fn toolbar(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.pad(12.0, 7.0, |ui| {
        ui.row_fill(8.0, |ui| {
            let name = model
                .path
                .as_deref()
                .map(|path| path.rsplit('/').next().unwrap_or(path).trim_end_matches(".md"))
                .unwrap_or("笔记");
            ui.text(name.to_string(), TextRole::Label);
            if !model.can_write {
                ui.text("只读", TextRole::Caption);
            }
            ui.grow();
            status(ui, model);
            ui.icon_button_named("note:save", "save", 14.0, 26.0, ColorToken::TextMuted, model.can_write && model.dirty);
            ui.icon_button_named("note:history", "history", 14.0, 26.0, ColorToken::TextMuted, model.path.is_some());
            mode_buttons(ui, model);
        });
    });
}

fn status(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    if let Some(error) = &model.save_error {
        ui.text(short(error, "保存失败"), TextRole::Caption);
    } else if model.dirty {
        ui.text("未保存", TextRole::Caption);
    }
}

fn mode_buttons(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let accent = ColorToken::Accent;
    let muted = ColorToken::TextMuted;
    let color = |mode: Mode| if model.mode == mode { accent } else { muted };
    let enabled = model.can_write;
    ui.row_fit(2.0, |ui| {
        ui.icon_button_named("note:mode:read", "eye", 14.0, 26.0, color(Mode::Read), true);
        ui.icon_button_named(
            "note:mode:edit",
            "pencil",
            14.0,
            26.0,
            color(Mode::Edit),
            enabled,
        );
        ui.icon_button_named(
            "note:mode:source",
            "code",
            14.0,
            26.0,
            color(Mode::Source),
            enabled,
        );
    });
}

fn read_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.scroll(READ_SCROLL, 0.0, |ui| {
        ui.pad(16.0, 10.0, |ui| {
            let body = &model.text[model.body_start.min(model.text.len())..];
            ui.markdown(body);
        });
    });
}

fn edit_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let body_size = ui.role_size(TextRole::Body);
    ui.scroll("note:preview-scroll", 4.0, |ui| {
        ui.pad(16.0, 10.0, |ui| {
            if model.blocks.is_empty() {
                ui.text_area_plain_auto(
                    EMPTY_EDIT,
                    &model.text[model.body_start.min(model.text.len())..],
                    40.0,
                    600.0,
                    body_size,
                    Edges::symmetric(0.0, 0.0),
                );
                return;
            }
            for (index, _) in model.blocks.iter().enumerate() {
                if model.active_block == Some(index) {
                    let source = model.active_source().unwrap_or("");
                    ui.text_area_plain_auto(
                        block_id(index),
                        source,
                        40.0,
                        600.0,
                        body_size,
                        Edges::symmetric(0.0, 2.0),
                    );
                } else {
                    let (from, to) = model.block_source(index).unwrap_or((0, 0));
                    ui.markdown_named(block_id(index), &model.text[from..to], Vec::new());
                }
                ui.spacer(8.0);
            }
        });
    });
}

fn source_view(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let body_size = ui.role_size(TextRole::Body);
    ui.scroll(SOURCE_SCROLL, 0.0, |ui| {
        ui.pad(16.0, 10.0, |ui| {
            ui.text_area_plain_auto(
                SOURCE,
                &model.text,
                40.0,
                4000.0,
                body_size,
                Edges::symmetric(0.0, 0.0),
            );
        });
    });
}

fn backlinks(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    ui.pad(12.0, 6.0, |ui| {
        ui.row_fit(6.0, |ui| {
            ui.icon_named("unlink", 12.0, ColorToken::TextMuted);
            ui.text(
                format!("反链 ({})", model.backlinks.len()),
                TextRole::Caption,
            );
        });
        for (index, link) in model.backlinks.iter().enumerate() {
            ui.click_row(
                format!("note:backlink:{index}"),
                Edges::symmetric(4.0, 2.0),
                3.0,
                false,
                true,
                |ui| {
                    ui.row_fit(6.0, |ui| {
                        ui.icon_named("file-text", 12.0, ColorToken::TextMuted);
                        ui.text(link.title.clone(), TextRole::Caption);
                    });
                },
            );
        }
    });
}

fn short(text: &str, fallback: &str) -> String {
    if text.trim().is_empty() {
        fallback.to_string()
    } else if text.chars().count() > 40 {
        let mut out: String = text.chars().take(39).collect();
        out.push('…');
        out
    } else {
        text.to_string()
    }
}
