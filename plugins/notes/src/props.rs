//! 属性面板：frontmatter 的类型化渲染与槽位编辑。
//! 改动只在模型里持草稿，提交时整篇回写并交宿主合并落盘。

use atelyx_format::frontmatter::{self, PropType, Yaml};
use atelyx_plugin_protocol::paint::ColorToken;
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::{NoteModel, Slot};

/// 属性槽位的单行输入框 id（同时至多一个槽在编辑）。
pub const KEY_INPUT: &str = "props:input";
/// 候选清单的滚动容器 id（↑↓ 巡航时由插件把高亮行滚进可视区）。
pub const SUGGEST_SCROLL: &str = "props:suggest-scroll";

/// 徽章键：值渲染为 `#` 胶囊流。
const BADGE_KEYS: &[&str] = &["tags"];
/// 常用键名建议（聚焦即展示、键入子串过滤、排除已有）。
const COMMON_KEYS: &[&str] = &[
    "tags", "aliases", "cssclasses", "status", "priority", "type", "author", "source", "url",
    "created", "updated", "summary",
];
/// 候选清单可视行数上限。
const SUGGEST_MAX_ROWS: usize = 8;
/// 候选行高（与宿主候选卡同档）。
const SUGGEST_ROW_H: f32 = 24.0;
/// 新属性的占位键名。
const PLACEHOLDER_KEY: &str = "property";

/// 面板渲染：没有 frontmatter 块时整块不出现；YAML 解析失败只显示红条并锁编辑。
pub fn panel(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let Some((pairs, ok)) = &model.props.data else { return };
    ui.pad(16.0, 6.0, |ui| {
        ui.column(2.0, |ui| {
            if !pairs.is_empty() {
                let chevron = if model.props.collapsed { "chevron-right" } else { "chevron-down" };
                ui.click_row(
                    "props:toggle",
                    Edges::all(2.0),
                    3.0,
                    false,
                    true,
                    |ui| {
                        ui.row_fit(6.0, |ui| {
                            ui.icon_named(chevron, 12.0, ColorToken::TextMuted);
                            let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextSecondary);
                            ui.text_styled_fit(format!("笔记属性 ({})", pairs.len()), style);
                        });
                    },
                );
            }
            if model.props.collapsed {
                return;
            }
            if !ok {
                ui.pad(0.0, 2.0, |ui| {
                    ui.row_fit(8.0, |ui| {
                        ui.icon_named("triangle-alert", 13.0, ColorToken::Danger);
                        let style = TextStyle::new(TextRole::Caption).color(ColorToken::Danger);
                        ui.text_styled_fit("YAML 格式错误，属性面板暂不可编辑", style);
                    });
                });
                return;
            }
            for (index, (key, value)) in pairs.iter().enumerate() {
                row(ui, model, index, key, value);
                suggestions(ui, model, index);
            }
            if let Some(error) = &model.props.error {
                let style = TextStyle::new(TextRole::Micro).color(ColorToken::Danger);
                ui.text_styled_fit(error.clone(), style);
            }
            add_area(ui, model);
        });
    });
}

fn row(ui: &mut Ui<'_, '_>, model: &NoteModel, index: usize, key: &str, value: &Yaml) {
    let kind = frontmatter::infer_type(is_badge_key(key), value);
    let key_id = format!("props:key:{index}");
    let delete_id = format!("props:delete:{index}");
    // 整段键名区（含删除钮占位）作为悬停区：指针从键名移到删除钮途中不会闪断
    let row_id = format!("props:keyrow:{index}");
    let hover_key = model.can_write
        && (ui.hovered(&row_id) || ui.hovered(KEY_INPUT) || ui.hovered(&delete_id));
    ui.row(6.0, |ui| {
        ui.icon_button_named(
            format!("props:type:{index}"),
            type_icon(kind),
            13.0,
            20.0,
            ColorToken::TextMuted,
            true,
        );
        ui.click_box(
            row_id,
            BoxWidth::Fixed(96.0),
            Edges::symmetric(0.0, 0.0),
            3.0,
            false,
            true,
            |ui| {
                ui.row_fit(2.0, |ui| {
                    if model.props.editing == Some(Slot::Key(index)) {
                        ui.text_input_inline_sized(KEY_INPUT, model.props.draft.clone(), "键名", 80.0);
                    } else {
                        ui.click_box(
                            key_id,
                            BoxWidth::Fixed(80.0),
                            Edges::symmetric(4.0, 3.0),
                            3.0,
                            false,
                            true,
                            |ui| {
                                let style =
                                    TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted);
                                ui.text_styled(key.to_string(), style);
                            },
                        );
                    }
                    if hover_key {
                        ui.icon_button_named(delete_id, "x", 10.0, 14.0, ColorToken::Danger, true);
                    } else {
                        ui.spacer_width(14.0);
                    }
                });
            },
        );
        value_cell(ui, model, index, value, kind);
    });
}

fn value_cell(ui: &mut Ui<'_, '_>, model: &NoteModel, index: usize, value: &Yaml, kind: PropType) {
    match value {
        Yaml::Bool(flag) => {
            let label = if *flag { "true" } else { "false" };
            let width = ui.text_width(label, TextRole::Micro) + 12.0;
            ui.click_box(
                format!("props:toggle-bool:{index}"),
                BoxWidth::Fixed(width),
                Edges::symmetric(6.0, 2.0),
                3.0,
                false,
                model.can_write,
                |ui| {
                    let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextSecondary);
                    ui.text_styled_fit(label.to_string(), style);
                },
            );
        }
        Yaml::Seq(items) if kind == PropType::Tag => {
            ui.row_fit(4.0, |ui| {
                for (item, value) in items.iter().enumerate() {
                    if model.props.editing == Some(Slot::Item(index, item)) {
                        ui.text_input_inline_sized(KEY_INPUT, model.props.draft.clone(), "标签", 90.0);
                        continue;
                    }
                    tag_pill(ui, model, index, item, value);
                }
                if model.props.editing == Some(Slot::NewItem(index)) {
                    ui.text_input_inline_sized(KEY_INPUT, model.props.draft.clone(), "标签", 90.0);
                } else {
                    tiny_add(ui, model, format!("props:additem:{index}"), true);
                }
            });
        }
        Yaml::Seq(items) => {
            ui.column(2.0, |ui| {
                for (item, value) in items.iter().enumerate() {
                    if model.props.editing == Some(Slot::Item(index, item)) {
                        ui.text_input_inline_sized(KEY_INPUT, model.props.draft.clone(), "项", 200.0);
                        continue;
                    }
                    let item_id = format!("props:item:{index}:{item}");
                    let delete_id = format!("props:delete-item:{index}:{item}");
                    let row_id = format!("props:itemrow:{index}:{item}");
                    let lit = model.can_write && (ui.hovered(&row_id) || ui.hovered(&delete_id));
                    let editable = scalar_text(value).is_some();
                    ui.click_box(
                        row_id,
                        BoxWidth::Fill,
                        Edges::symmetric(0.0, 0.0),
                        3.0,
                        false,
                        true,
                        |ui| {
                            ui.row(4.0, |ui| {
                                ui.icon_named("clock", 11.0, ColorToken::TextMuted);
                                if editable {
                                    ui.click_box(
                                        item_id,
                                        BoxWidth::Fill,
                                        Edges::symmetric(4.0, 1.0),
                                        3.0,
                                        false,
                                        true,
                                        |ui| {
                                            let style = TextStyle::new(TextRole::Caption)
                                                .color(ColorToken::Text);
                                            ui.text_styled(yaml_text(value), style);
                                        },
                                    );
                                } else {
                                    let style = TextStyle::new(TextRole::Caption)
                                        .color(ColorToken::TextMuted);
                                    ui.text_styled("（嵌套）".to_string(), style);
                                }
                                if lit {
                                    ui.icon_button_named(
                                        delete_id,
                                        "x",
                                        9.0,
                                        13.0,
                                        ColorToken::Danger,
                                        true,
                                    );
                                } else {
                                    ui.spacer_width(13.0);
                                }
                            });
                        },
                    );
                }
                if model.props.editing == Some(Slot::NewItem(index)) {
                    ui.text_input_inline_sized(KEY_INPUT, model.props.draft.clone(), "项", 200.0);
                } else {
                    tiny_add(ui, model, format!("props:additem:{index}"), false);
                }
            });
        }
        Yaml::Str(text) => {
            if model.props.editing == Some(Slot::Value(index)) {
                ui.text_input_inline(KEY_INPUT, model.props.draft.clone(), "属性值");
            } else {
                let empty = text.is_empty();
                let shown = if empty { "（空）".to_string() } else { text.clone() };
                ui.click_box(
                    format!("props:value:{index}"),
                    BoxWidth::Fill,
                    Edges::symmetric(4.0, 3.0),
                    3.0,
                    false,
                    true,
                    |ui| {
                        let style = TextStyle::new(TextRole::Caption).color(if empty {
                            ColorToken::TextMuted
                        } else {
                            ColorToken::Text
                        });
                        ui.text_styled(shown, style);
                    },
                );
            }
        }
        Yaml::Map(_) => {
            let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted);
            ui.text_styled("（嵌套）".to_string(), style);
        }
        other => {
            ui.click_box(
                format!("props:value:{index}"),
                BoxWidth::Fill,
                Edges::symmetric(4.0, 3.0),
                3.0,
                false,
                true,
                |ui| {
                    let style = TextStyle::new(TextRole::Caption).color(ColorToken::Text);
                    ui.text_styled(yaml_text(other), style);
                },
            );
        }
    }
}

/// 标签胶囊（强调浅底 + 圆角）：文字与删除钮都嵌在同一底色块里，删除钮悬停才显、宽度预留。
fn tag_pill(ui: &mut Ui<'_, '_>, model: &NoteModel, index: usize, item: usize, value: &Yaml) {
    let label = format!("#{}", yaml_text(value));
    let item_id = format!("props:item:{index}:{item}");
    let delete_id = format!("props:delete-item:{index}:{item}");
    let row_id = format!("props:pill:{index}:{item}");
    let lit = model.can_write && (ui.hovered(&row_id) || ui.hovered(&delete_id));
    let delete_w = if model.can_write { 12.0 } else { 0.0 };
    let width = ui.text_width(&label, TextRole::Micro) + delete_w + 14.0;
    ui.boxed(
        BoxWidth::Fixed(width),
        BoxStyle::new(Some(ColorToken::AccentSoft), None, 9.0),
        Edges::symmetric(5.0, 2.0),
        1.0,
        |ui| {
            ui.click_box(
                row_id,
                BoxWidth::Fixed(width - 10.0),
                Edges::symmetric(0.0, 0.0),
                0.0,
                false,
                true,
                |ui| {
                    ui.row(1.0, |ui| {
                        ui.click_box(
                            item_id,
                            BoxWidth::Fixed(width - 11.0 - delete_w),
                            Edges::symmetric(0.0, 0.0),
                            0.0,
                            false,
                            true,
                            |ui| {
                                let style =
                                    TextStyle::new(TextRole::Micro).color(ColorToken::Accent);
                                ui.text_styled_fit(label, style);
                            },
                        );
                        if lit {
                            ui.icon_button_named(
                                delete_id,
                                "x",
                                8.0,
                                12.0,
                                ColorToken::Danger,
                                true,
                            );
                        } else if model.can_write {
                            ui.spacer_width(12.0);
                        }
                    });
                },
            );
        },
    );
}

/// 数组行的「添加」入口：标签行给加号，普通列表给极简文字。
fn tiny_add(ui: &mut Ui<'_, '_>, model: &NoteModel, id: String, icon: bool) {
    let enabled = model.can_write && model.props.editing.is_none();
    let label = "添加".to_string();
    let width = if icon {
        18.0
    } else {
        ui.text_width(&label, TextRole::Micro) + 8.0
    };
    ui.click_box(
        id,
        BoxWidth::Fixed(width),
        Edges::symmetric(4.0, if icon { 3.5 } else { 2.5 }),
        3.0,
        false,
        enabled,
        |ui| {
            if icon {
                ui.icon_named("plus", 11.0, ColorToken::TextMuted);
            } else {
                let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
                ui.text_styled_fit(label, style);
            }
        },
    );
}

/// 底部「添加属性」入口：常驻；有槽正在编辑时不可再点。
fn add_area(ui: &mut Ui<'_, '_>, model: &NoteModel) {
    let enabled = model.can_write && model.props.editing.is_none();
    let width = ui.text_width("添加属性", TextRole::Micro) + 11.0 + 4.0 + 8.0;
    ui.click_box(
        "props:add",
        BoxWidth::Fixed(width),
        Edges::symmetric(4.0, 3.0),
        3.0,
        false,
        enabled,
        |ui| {
            ui.row_fit(4.0, |ui| {
                ui.icon_named("plus", 11.0, ColorToken::TextMuted);
                let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
                ui.text_styled_fit("添加属性".to_string(), style);
            });
        },
    );
}

fn suggestions(ui: &mut Ui<'_, '_>, model: &NoteModel, index: usize) {
    let Some(view) = view_for_row(model, index) else { return };
    if !view.pool {
        return;
    }
    let visible = view.items.len().min(SUGGEST_MAX_ROWS);
    let height = if visible == 0 { SUGGEST_ROW_H } else { visible as f32 * SUGGEST_ROW_H };
    let items = view.items;
    let tags = view.tags;
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceMuted), Some(ColorToken::Border), 4.0),
        Edges::symmetric(0.0, 2.0),
        0.0,
        |ui| {
            let rows = |ui: &mut Ui<'_, '_>| {
                if items.is_empty() {
                    let text = if tags { "没有匹配的标签" } else { "没有匹配的键名" };
                    let pad = (SUGGEST_ROW_H - ui.text_height(TextRole::Caption)) / 2.0;
                    ui.click_box(
                        "props:suggest:none",
                        BoxWidth::Fill,
                        Edges::symmetric(6.0, pad.max(0.0)),
                        3.0,
                        false,
                        false,
                        |ui| {
                            let style = TextStyle::new(TextRole::Caption).color(ColorToken::TextMuted);
                            ui.text_styled_fit(text.to_string(), style);
                        },
                    );
                    return;
                }
                let active = model.props.suggest_index.min(items.len().saturating_sub(1));
                for (item, label) in items.iter().enumerate() {
                    let selected = model.props.suggest_armed && item == active;
                    let pad = (SUGGEST_ROW_H - ui.text_height(TextRole::Caption)) / 2.0;
                    let id = format!("props:suggest:{item}");
                    ui.click_box(
                        id,
                        BoxWidth::Fill,
                        Edges::symmetric(6.0, pad.max(0.0)),
                        3.0,
                        selected,
                        true,
                        |ui| {
                            if tags {
                                ui.row_fit(5.0, |ui| {
                                    ui.icon_named(
                                        "tag",
                                        12.0,
                                        if selected { ColorToken::Accent } else { ColorToken::TextMuted },
                                    );
                                    let style = TextStyle::new(TextRole::Caption).color(if selected {
                                        ColorToken::Accent
                                    } else {
                                        ColorToken::Text
                                    });
                                    ui.text_styled_fit(format!("#{label}"), style);
                                });
                            } else {
                                let style = TextStyle::new(TextRole::Caption).color(if selected {
                                    ColorToken::Accent
                                } else {
                                    ColorToken::Text
                                });
                                ui.text_styled(label.clone(), style);
                            }
                        },
                    );
                }
            };
            if items.len() > SUGGEST_MAX_ROWS {
                ui.scroll_max(SUGGEST_SCROLL, height, 0.0, rows);
            } else {
                ui.column(0.0, rows);
            }
        },
    );
}

struct SuggestView {
    items: Vec<String>,
    tags: bool,
    pool: bool,
}

fn view_for_row(model: &NoteModel, index: usize) -> Option<SuggestView> {
    let (pairs, _) = model.props.data.as_ref()?;
    let query = model.props.suggest_query.as_str();
    match model.props.editing.as_ref()? {
        Slot::Key(row) if *row == index => {
            let existing: Vec<String> = pairs.iter().map(|(key, _)| key.clone()).collect();
            Some(filter_keys(&existing, query))
        }
        Slot::Item(row, _) | Slot::NewItem(row) if *row == index => {
            let key = pairs.get(*row)?.0.as_str();
            if !is_badge_key(key) {
                return None;
            }
            let mut existing: Vec<String> = pairs
                .iter()
                .filter(|(key, _)| is_badge_key(key))
                .flat_map(|(_, value)| match value {
                    Yaml::Seq(items) => items.iter().map(yaml_text).collect::<Vec<_>>(),
                    other => vec![yaml_text(other)],
                })
                .collect();
            existing.extend(split_comma_list(query));
            Some(filter_tags(&model.tag_vocab, &existing, query))
        }
        _ => None,
    }
}

fn filter_keys(existing: &[String], query: &str) -> SuggestView {
    let q = query.trim().to_lowercase();
    let mut items = Vec::new();
    let mut pool = false;
    for key in COMMON_KEYS {
        if existing.iter().any(|used| used.as_str() == *key) {
            continue;
        }
        pool = true;
        if q.is_empty() || key.to_lowercase().contains(&q) {
            items.push((*key).to_string());
        }
    }
    SuggestView { items, tags: false, pool }
}

fn filter_tags(vocab: &[String], used: &[String], query: &str) -> SuggestView {
    let last = query.rsplit([',', '，']).next().unwrap_or("").trim().to_lowercase();
    let mut items = Vec::new();
    let mut pool = false;
    for tag in vocab {
        if used.iter().any(|value| value == tag) {
            continue;
        }
        pool = true;
        if last.is_empty() || tag.to_lowercase().contains(&last) {
            items.push(tag.clone());
        }
    }
    SuggestView { items, tags: true, pool }
}

/// 当前编辑槽的候选（键盘巡航 / 点击选中与渲染同一口径）。
pub fn suggestion_items(model: &NoteModel) -> Vec<String> {
    let index = match model.props.editing.as_ref() {
        Some(Slot::Key(row)) => *row,
        Some(Slot::Item(row, _) | Slot::NewItem(row)) => *row,
        _ => return Vec::new(),
    };
    view_for_row(model, index).map(|view| view.items).unwrap_or_default()
}

/// ↑↓ 巡航：首次点亮首 / 尾项；返回是否发生了移动。
pub fn move_suggestion(model: &mut NoteModel, delta: i32) -> bool {
    let count = suggestion_items(model).len();
    if count == 0 {
        return false;
    }
    let last = count - 1;
    model.props.suggest_index = if model.props.suggest_armed {
        (model.props.suggest_index.min(last) as i32 + delta).clamp(0, last as i32) as usize
    } else if delta > 0 {
        0
    } else {
        last
    };
    model.props.suggest_armed = true;
    true
}

/// 候选清单纵向滚动偏移（高亮行越过可视区后跟随）。
pub fn suggest_scroll_offset(model: &NoteModel) -> f32 {
    let count = suggestion_items(model).len();
    if count <= SUGGEST_MAX_ROWS {
        return 0.0;
    }
    let first = model
        .props
        .suggest_index
        .saturating_sub(SUGGEST_MAX_ROWS - 1)
        .min(count - SUGGEST_MAX_ROWS);
    first as f32 * SUGGEST_ROW_H
}

pub fn reset_suggest(model: &mut NoteModel) {
    model.props.suggest_query.clear();
    model.props.suggest_index = 0;
    model.props.suggest_armed = false;
}

/// 放弃在途槽编辑（不提交草稿）：删除类动作按渲染下标执行，先取消可避免提交改行后下标漂移。
pub fn cancel_edit(model: &mut NoteModel) {
    model.props.editing = None;
    model.props.draft.clear();
    model.props.error = None;
    reset_suggest(model);
    model.refresh_props();
}

fn is_badge_key(key: &str) -> bool {
    BADGE_KEYS.contains(&key)
}

fn type_icon(kind: PropType) -> &'static str {
    match kind {
        PropType::Text => "type",
        PropType::Tag => "tag",
        PropType::Date => "calendar-days",
        PropType::Number => "hash",
        PropType::Boolean => "toggle-left",
        PropType::List => "list",
    }
}

fn type_key(kind: PropType) -> &'static str {
    match kind {
        PropType::Text => "text",
        PropType::Tag => "tag",
        PropType::Date => "date",
        PropType::Number => "number",
        PropType::Boolean => "boolean",
        PropType::List => "list",
    }
}

fn parse_type(key: &str) -> Option<PropType> {
    Some(match key {
        "text" => PropType::Text,
        "tag" => PropType::Tag,
        "date" => PropType::Date,
        "number" => PropType::Number,
        "boolean" => PropType::Boolean,
        "list" => PropType::List,
        _ => return None,
    })
}

/// 类型菜单：六种类型（徽章键锁标签，其余键标签不可选）。
pub fn type_menu(ui: &mut Ui<'_, '_>, model: &NoteModel, index: usize, left: f32, bottom: f32) {
    const TYPES: [(PropType, &str, &str); 6] = [
        (PropType::Text, "type", "文本"),
        (PropType::Tag, "tag", "标签"),
        (PropType::Date, "calendar-days", "日期"),
        (PropType::Number, "hash", "数字"),
        (PropType::Boolean, "toggle-left", "布尔"),
        (PropType::List, "list", "列表"),
    ];
    let Some((pairs, _)) = &model.props.data else { return };
    let Some((key, value)) = pairs.get(index) else { return };
    let badge = is_badge_key(key);
    let current = frontmatter::infer_type(badge, value);
    let width = 132.0;
    ui.overlay(
        atelyx_plugin_sdk::frame_ui::Rect::new(left, bottom, width, 6.0 + 26.0 * TYPES.len() as f32),
        |menu| {
            menu.boxed(
                BoxWidth::Fixed(width),
                BoxStyle::new(Some(ColorToken::SurfaceOverlay), Some(ColorToken::Border), 6.0),
                Edges::all(6.0),
                0.0,
                |menu| {
                    for (kind, icon, label) in TYPES {
                        let locked = if badge { kind != PropType::Tag } else { kind == PropType::Tag };
                        let selected = current == kind;
                        menu.click_row(
                            format!("props:typepick:{index}:{}", type_key(kind)),
                            Edges::symmetric(6.0, 3.0),
                            3.0,
                            selected,
                            !locked,
                            |ui| {
                                ui.row_fit(6.0, |ui| {
                                    ui.icon_named(icon, 13.0, ColorToken::TextMuted);
                                    let style = TextStyle::new(TextRole::Caption).color(if selected {
                                        ColorToken::Accent
                                    } else {
                                        ColorToken::TextSecondary
                                    });
                                    ui.text_styled_fit(label, style);
                                    if selected {
                                        ui.grow();
                                        let style = TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted);
                                        ui.text_styled_fit("当前", style);
                                    }
                                });
                            },
                        );
                    }
                },
            );
        },
    );
}

/// 点击处理：返回 true 表示需要把键盘焦点交给属性输入框。
pub fn on_click(model: &mut NoteModel, id: &str) -> bool {
    if id == "props:toggle" {
        model.props.collapsed = !model.props.collapsed;
        return false;
    }
    if !model.can_write {
        return false;
    }
    if id == "props:add" {
        return false;
    }
    if let Some(rest) = id.strip_prefix("props:key:") {
        let Ok(index) = rest.parse::<usize>() else { return false };
        let Some((pairs, _)) = &model.props.data else { return false };
        let Some((key, _)) = pairs.get(index) else { return false };
        begin_slot(model, Slot::Key(index), key.clone());
        return true;
    }
    if let Some(rest) = id.strip_prefix("props:value:") {
        let Ok(index) = rest.parse::<usize>() else { return false };
        let Some((pairs, _)) = &model.props.data else { return false };
        let Some((_, value)) = pairs.get(index) else { return false };
        let text = yaml_text(value);
        begin_slot(model, Slot::Value(index), text);
        return true;
    }
    if let Some(rest) = id.strip_prefix("props:item:") {
        let Some((row, item)) = rest.split_once(':') else { return false };
        let (Ok(row), Ok(item)) = (row.parse::<usize>(), item.parse::<usize>()) else {
            return false;
        };
        let Some((pairs, _)) = &model.props.data else { return false };
        let Some((_, Yaml::Seq(items))) = pairs.get(row) else { return false };
        let Some(value) = items.get(item) else { return false };
        if scalar_text(value).is_none() {
            return false;
        }
        begin_slot(model, Slot::Item(row, item), yaml_text(value));
        return true;
    }
    if let Some(rest) = id.strip_prefix("props:additem:") {
        let Ok(index) = rest.parse::<usize>() else { return false };
        let Some((pairs, _)) = &model.props.data else { return false };
        let Some((_, Yaml::Seq(_))) = pairs.get(index) else { return false };
        begin_slot(model, Slot::NewItem(index), String::new());
        return true;
    }
    if let Some(rest) = id.strip_prefix("props:type:") {
        let Ok(index) = rest.parse::<usize>() else { return false };
        model.props.type_menu = Some(index);
        return false;
    }
    false
}

fn begin_slot(model: &mut NoteModel, slot: Slot, draft: String) {
    model.props.editing = Some(slot);
    model.props.draft = draft;
    model.props.error = None;
    reset_suggest(model);
}

/// 追加一条空属性并进入键名编辑；返回是否应把焦点交给输入框。
/// 调用方随即把占位键写进正文，因此 Esc 取消编辑后这条空属性仍留在文件里。
pub fn add_property(model: &mut NoteModel) -> bool {
    if !model.can_write || model.props.editing.is_some() {
        return false;
    }
    let (mut pairs, ok) = match model.props.data.clone() {
        Some((pairs, ok)) => (pairs, ok),
        None => (Vec::new(), true),
    };
    if !ok {
        return false;
    }
    let key = unique_key(&pairs);
    pairs.push((key.clone(), Yaml::Str(String::new())));
    model.props.data = Some((pairs.clone(), true));
    model.props.collapsed = false;
    begin_slot(model, Slot::Key(pairs.len() - 1), key);
    true
}

/// 占位键先写进正文（返回新全文）。
pub fn text_with_placeholder(model: &NoteModel) -> Option<String> {
    let (pairs, _) = model.props.data.as_ref()?;
    Some(model.text_with_props(pairs.clone()))
}

/// Enter：候选已用 ↑↓ 高亮则选中候选，否则按方向提交并步进。
pub fn submit(model: &mut NoteModel, dir: i32) -> (Option<String>, bool) {
    if model.props.suggest_armed {
        let items = suggestion_items(model);
        let index = model.props.suggest_index.min(items.len().saturating_sub(1));
        if let Some(text) = items.into_iter().nth(index) {
            return pick(model, text, dir);
        }
    }
    commit(model, dir)
}

/// 点击候选：替换草稿后提交（鼠标选中不步进）。
pub fn pick_suggestion(model: &mut NoteModel, index: usize) -> (Option<String>, bool) {
    let items = suggestion_items(model);
    match items.into_iter().nth(index) {
        Some(text) => pick(model, text, 0),
        None => (None, false),
    }
}

fn pick(model: &mut NoteModel, mut text: String, dir: i32) -> (Option<String>, bool) {
    model.props.suggest_armed = false;
    if matches!(model.props.editing, Some(Slot::Item(..) | Slot::NewItem(_)) | Some(Slot::Key(_))) {
        // 标签项草稿可能带逗号分隔的多段：只替换最后一段，前面的段保留
        let row = match model.props.editing {
            Some(Slot::Item(row, _) | Slot::NewItem(row)) => Some(row),
            _ => None,
        };
        let tag_row = row.is_some_and(|row| {
            model
                .props
                .data
                .as_ref()
                .and_then(|(pairs, _)| pairs.get(row))
                .is_some_and(|(key, _)| is_badge_key(key))
        });
        if tag_row {
            if let Some(at) = model.props.draft.rfind(|ch| ch == ',' || ch == '，') {
                text = format!("{}{}", &model.props.draft[..=at], text);
            }
        }
        model.props.draft = text;
    }
    commit(model, dir)
}

/// 提交当前槽位并按方向步进：返回（新全文，是否已切到下一槽）。
pub fn commit(model: &mut NoteModel, dir: i32) -> (Option<String>, bool) {
    let Some(slot) = model.props.editing.clone() else { return (None, false) };
    let Some((pairs, _)) = model.props.data.clone() else {
        reset_suggest(model);
        model.props.editing = None;
        model.props.draft.clear();
        return (None, false);
    };
    let mut pairs = pairs;
    if !slot_valid(&pairs, &slot) {
        reset_suggest(model);
        model.props.editing = None;
        model.props.draft.clear();
        model.props.error = None;
        return (None, false);
    }
    let draft = model.props.draft.trim().to_string();
    if let Slot::Key(index) = slot {
        if !draft.is_empty()
            && pairs[index].0 != draft
            && pairs.iter().enumerate().any(|(i, (key, _))| i != index && key == &draft)
        {
            model.props.error = Some(format!("属性「{draft}」已存在，未写入"));
            reset_suggest(model);
            model.props.editing = None;
            model.props.draft.clear();
            return (None, false);
        }
    }
    let outcome = commit_slot_data(&mut pairs, &slot, &draft);
    model.props.draft.clear();
    model.props.error = None;
    reset_suggest(model);
    model.props.editing = None;
    let text = if outcome.changed {
        let text = model.text_with_props(pairs.clone());
        model.props.data = Some((pairs.clone(), true));
        Some(text)
    } else {
        model.refresh_props();
        None
    };
    if outcome.deleted || dir == 0 {
        return (text, false);
    }
    let Some(anchor) = anchor_slot(&slot, outcome.item_span) else { return (text, false) };
    let slots = flatten_slots(&pairs);
    let target = match slots.iter().position(|slot| *slot == anchor) {
        Some(index) if dir > 0 => slots.get(index + 1).cloned(),
        Some(index) => index.checked_sub(1).and_then(|prev| slots.get(prev).cloned()),
        None => None,
    };
    if let Some(next) = target {
        model.props.editing = Some(next.clone());
        model.props.draft = slot_text(&pairs, &next);
        (text, true)
    } else {
        (text, false)
    }
}

/// 类型菜单选项的点击（命中即改值并提交）。
pub fn on_type_pick(model: &mut NoteModel, id: &str) -> Option<String> {
    let rest = id.strip_prefix("props:typepick:")?;
    let (row, kind) = rest.split_once(':')?;
    let row = row.parse::<usize>().ok()?;
    let kind = parse_type(kind)?;
    let (pairs, _) = model.props.data.as_mut()?;
    let value = &mut pairs.get_mut(row)?.1;
    let Some(converted) = frontmatter::convert_value(value, kind) else {
        model.props.error = Some("无法转换为该类型".to_string());
        return None;
    };
    *value = converted;
    model.props.type_menu = None;
    model.props.error = None;
    let pairs = pairs.clone();
    Some(model.text_with_props(pairs))
}

/// 直接改值类点击（删行 / 删项 / 翻转布尔）：返回新全文（无变化返回 None）。
pub fn quick_action(model: &mut NoteModel, id: &str) -> Option<String> {
    if !model.can_write {
        return None;
    }
    let (pairs, ok) = model.props.data.clone()?;
    if !ok {
        return None;
    }
    let mut pairs = pairs;
    if let Some(rest) = id.strip_prefix("props:delete:") {
        let index = rest.parse::<usize>().ok()?;
        if index >= pairs.len() {
            return None;
        }
        pairs.remove(index);
    } else if let Some(rest) = id.strip_prefix("props:delete-item:") {
        let (row, item) = rest.split_once(':')?;
        let (row, item) = (row.parse::<usize>().ok()?, item.parse::<usize>().ok()?);
        let (_, value) = pairs.get_mut(row)?;
        let Yaml::Seq(items) = value else { return None };
        if item >= items.len() {
            return None;
        }
        items.remove(item);
    } else if let Some(rest) = id.strip_prefix("props:toggle-bool:") {
        let index = rest.parse::<usize>().ok()?;
        let (_, value) = pairs.get_mut(index)?;
        let Yaml::Bool(flag) = value else { return None };
        *flag = !*flag;
    } else {
        return None;
    }
    model.props.editing = None;
    model.props.draft.clear();
    model.props.error = None;
    reset_suggest(model);
    Some(model.text_with_props(pairs))
}

/// 值区文本：序列按「，」连接，其余标量取原文本。
pub fn yaml_text(value: &Yaml) -> String {
    match value {
        Yaml::Null => String::new(),
        Yaml::Bool(flag) => if *flag { "true" } else { "false" }.to_string(),
        Yaml::Num(number) => {
            if number.fract() == 0.0 {
                format!("{}", *number as i64)
            } else {
                number.to_string()
            }
        }
        Yaml::Date(date) => date.clone(),
        Yaml::Str(text) => text.clone(),
        Yaml::Seq(items) => items.iter().map(yaml_text).collect::<Vec<_>>().join("，"),
        Yaml::Map(_) => String::new(),
    }
}

fn slot_valid(pairs: &[(String, Yaml)], slot: &Slot) -> bool {
    match slot {
        Slot::Key(index) | Slot::Value(index) => *index < pairs.len(),
        Slot::Item(row, item) => pairs.get(*row).is_some_and(|(_, value)| {
            matches!(value, Yaml::Seq(items) if *item < items.len())
        }),
        Slot::NewItem(row) => pairs.get(*row).is_some_and(|(_, value)| matches!(value, Yaml::Seq(_))),
    }
}

fn slot_text(pairs: &[(String, Yaml)], slot: &Slot) -> String {
    match slot {
        Slot::Key(index) => pairs.get(*index).map(|(key, _)| key.clone()).unwrap_or_default(),
        Slot::Value(index) => pairs.get(*index).map(|(_, value)| yaml_text(value)).unwrap_or_default(),
        Slot::Item(row, item) => pairs
            .get(*row)
            .and_then(|(_, value)| match value {
                Yaml::Seq(items) => items.get(*item).map(yaml_text),
                _ => None,
            })
            .unwrap_or_default(),
        Slot::NewItem(_) => String::new(),
    }
}

/// 提交结果：`item_span` = 数组项提交后占用的下标区间（前进落点用）。
struct CommitOutcome {
    changed: bool,
    deleted: bool,
    item_span: Option<(usize, usize)>,
}

impl CommitOutcome {
    fn unchanged() -> Self {
        Self { changed: false, deleted: false, item_span: None }
    }

    fn removed() -> Self {
        Self { changed: true, deleted: true, item_span: None }
    }

    fn changed(span: Option<(usize, usize)>) -> Self {
        Self { changed: true, deleted: false, item_span: span }
    }
}

fn anchor_slot(slot: &Slot, item_span: Option<(usize, usize)>) -> Option<Slot> {
    Some(match slot {
        Slot::Key(_) | Slot::Value(_) => slot.clone(),
        Slot::Item(row, item) => match item_span {
            Some((first, last)) => Slot::Item(*row, last.max(first)),
            None => Slot::Item(*row, *item),
        },
        Slot::NewItem(_) => return None,
    })
}

/// 槽位提交：返回数据是否变化与槽是否被删空。
/// 草稿与原值一致 = 无变化；单值清空 = 留空属性（可再切类型）；键名 / 数组项清空 = 删；
/// 数组项含逗号 = 拆多项替换。
fn commit_slot_data(pairs: &mut Vec<(String, Yaml)>, slot: &Slot, draft: &str) -> CommitOutcome {
    match slot {
        Slot::Key(index) => {
            if draft == pairs[*index].0.trim() {
                return CommitOutcome::unchanged();
            }
            if draft.is_empty() {
                pairs.remove(*index);
                return CommitOutcome::removed();
            }
            pairs[*index].0 = draft.to_string();
            if draft == "tags" {
                if let Yaml::Str(text) = &pairs[*index].1 {
                    let items: Vec<Yaml> = split_comma_list(text).into_iter().map(Yaml::Str).collect();
                    pairs[*index].1 = if items.is_empty() { Yaml::Str(String::new()) } else { Yaml::Seq(items) };
                }
            }
            CommitOutcome::changed(None)
        }
        Slot::Value(index) => {
            let original = yaml_text(&pairs[*index].1);
            if original == draft {
                return CommitOutcome::unchanged();
            }
            let value = parse_like(&pairs[*index].1, draft);
            if yaml_text(&value) == original {
                return CommitOutcome::unchanged();
            }
            pairs[*index].1 = value;
            CommitOutcome::changed(None)
        }
        Slot::Item(row, item) => {
            let Yaml::Seq(items) = &mut pairs[*row].1 else { return CommitOutcome::unchanged() };
            let original = yaml_text(&items[*item]);
            if draft.is_empty() {
                if original.trim().is_empty() {
                    return CommitOutcome::unchanged();
                }
                items.remove(*item);
                return CommitOutcome::removed();
            }
            let parts = split_comma_list(draft);
            if parts.len() == 1 {
                if parts[0] == original.trim() {
                    return CommitOutcome::unchanged();
                }
                items[*item] = Yaml::Str(parts.into_iter().next().unwrap());
                return CommitOutcome::changed(Some((*item, *item)));
            }
            let count = parts.len();
            items.splice(*item..=*item, parts.into_iter().map(Yaml::Str));
            CommitOutcome::changed(Some((*item, *item + count - 1)))
        }
        Slot::NewItem(row) => {
            let Yaml::Seq(items) = &mut pairs[*row].1 else { return CommitOutcome::unchanged() };
            let parts = split_comma_list(draft);
            if parts.is_empty() {
                return CommitOutcome::unchanged();
            }
            let first = items.len();
            items.extend(parts.into_iter().map(Yaml::Str));
            CommitOutcome::changed(Some((first, items.len() - 1)))
        }
    }
}

/// 槽位线性序（键 → 可编辑值 / 数组项）；布尔与只读形态不入序。
fn flatten_slots(pairs: &[(String, Yaml)]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (index, (_, value)) in pairs.iter().enumerate() {
        slots.push(Slot::Key(index));
        match value {
            Yaml::Seq(items) => {
                for (item, value) in items.iter().enumerate() {
                    if scalar_text(value).is_some() {
                        slots.push(Slot::Item(index, item));
                    }
                }
            }
            Yaml::Bool(_) => {}
            _ => slots.push(Slot::Value(index)),
        }
    }
    slots
}

fn scalar_text(value: &Yaml) -> Option<String> {
    match value {
        Yaml::Str(_) | Yaml::Date(_) | Yaml::Num(_) => Some(yaml_text(value)),
        _ => None,
    }
}

/// 按当前值的类型解析草稿：布尔只认 true/false，数字保持数字，序列项恒为字符串。
fn parse_like(current: &Yaml, draft: &str) -> Yaml {
    match current {
        Yaml::Bool(_) => match draft.trim().to_ascii_lowercase().as_str() {
            "true" => Yaml::Bool(true),
            "false" => Yaml::Bool(false),
            _ => Yaml::Str(draft.to_string()),
        },
        Yaml::Date(_) if is_date(draft.trim()) => Yaml::Date(draft.trim().to_string()),
        Yaml::Num(_) => draft
            .trim()
            .parse::<f64>()
            .map(Yaml::Num)
            .unwrap_or_else(|_| Yaml::Str(draft.to_string())),
        _ => Yaml::Str(draft.to_string()),
    }
}

/// `YYYY-MM-DD` 形态的日期文本（与 frontmatter 日期类型同一档）。
fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
}

fn unique_key(pairs: &[(String, Yaml)]) -> String {
    let mut candidate = PLACEHOLDER_KEY.to_string();
    let mut seq = 2;
    while pairs.iter().any(|(key, _)| key == &candidate) {
        candidate = format!("{PLACEHOLDER_KEY}-{seq}");
        seq += 1;
    }
    candidate
}

/// 中英文逗号拆分（数组项与「改名落到徽章键」的数组展开规则）。
fn split_comma_list(text: &str) -> Vec<String> {
    text.split([',', '，'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(text: &str) -> NoteModel {
        let mut model = NoteModel::default();
        model.load("a.md".into(), text.into(), true);
        model
    }

    #[test]
    fn clearing_a_string_value_keeps_an_empty_property() {
        let mut note = model("---\na: x\n---\n正文");
        begin_slot(&mut note, Slot::Value(0), "x".into());
        note.props.draft.clear();
        let (text, next) = commit(&mut note, 0);
        assert!(!next);
        assert_eq!(text.as_deref(), Some("---\na: ''\n---\n正文"), "空字符串保留属性");
        assert_eq!(note.props.data.as_ref().unwrap().0[0].1, Yaml::Str(String::new()));
    }

    #[test]
    fn clearing_an_item_removes_it_and_does_not_step() {
        let mut note = model("---\ntags: [a, b]\n---\n正文");
        begin_slot(&mut note, Slot::Item(0, 0), "a".into());
        note.props.draft.clear();
        let (_, next) = commit(&mut note, 1);
        assert!(!next, "删空的槽不再前进");
        assert_eq!(note.props.data.as_ref().unwrap().0[0].1, Yaml::Seq(vec![Yaml::Str("b".into())]));
    }

    #[test]
    fn tab_steps_from_key_to_value() {
        let mut note = model("---\na: x\n---\n正文");
        begin_slot(&mut note, Slot::Key(0), "a".into());
        let (_, next) = commit(&mut note, 1);
        assert!(next);
        assert_eq!(note.props.editing, Some(Slot::Value(0)));
        assert_eq!(note.props.draft, "x");
    }

    #[test]
    fn duplicate_key_is_rejected_with_a_notice() {
        let mut note = model("---\na: x\nb: y\n---\n正文");
        begin_slot(&mut note, Slot::Key(0), "b".into());
        let (text, next) = commit(&mut note, 1);
        assert!(text.is_none() && !next);
        assert!(note.props.error.as_deref().unwrap().contains("已存在"));
        assert_eq!(note.props.editing, None);
    }

    #[test]
    fn adding_a_property_uses_the_placeholder_key_and_writes_immediately() {
        let mut note = model("---\na: x\n---\n正文");
        assert!(add_property(&mut note));
        let text = text_with_placeholder(&note).expect("有占位键");
        assert!(text.contains("property:"));
        assert_eq!(note.props.editing, Some(Slot::Key(1)));
    }

    #[test]
    fn key_suggestions_exclude_existing_keys_and_filter_by_draft() {
        let mut note = model("---\ntags: []\nstatus: x\n---\n正文");
        begin_slot(&mut note, Slot::Key(1), "cr".into());
        note.props.suggest_query = "cr".into();
        let items = suggestion_items(&note);
        assert!(!items.contains(&"status".to_string()), "已有键不重复提供");
        assert!(items.iter().any(|item| item == "created"), "按草稿过滤：{items:?}");
        assert!(items.iter().all(|item| item.to_lowercase().contains("cr")));
    }

    #[test]
    fn tag_suggestions_filter_the_last_comma_segment() {
        let mut note = model("---\ntags: [开发]\n---\n正文");
        note.tag_vocab = vec!["开发".into(), "开发工具".into(), "设计".into()];
        begin_slot(&mut note, Slot::NewItem(0), "开".into());
        note.props.suggest_query = "开".into();
        let items = suggestion_items(&note);
        assert_eq!(items, vec!["开发工具".to_string()], "排除已用标签并按最后一段过滤");
    }

    #[test]
    fn picking_a_tag_candidate_keeps_the_earlier_segments() {
        let mut note = model("---\ntags: []\n---\n正文");
        note.tag_vocab = vec!["开发".into(), "设计".into()];
        begin_slot(&mut note, Slot::NewItem(0), "开发,设".into());
        note.props.suggest_query = "开发,设".into();
        let (_, _) = pick_suggestion(&mut note, 0);
        let Yaml::Seq(items) = &note.props.data.as_ref().unwrap().0[0].1 else {
            panic!("tags 应为数组");
        };
        assert_eq!(items, &vec![Yaml::Str("开发".into()), Yaml::Str("设计".into())]);
    }
}
