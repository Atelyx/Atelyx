//! 笔记属性面板：frontmatter 的类型化渲染与槽位编辑（数据源 = core frontmatter 宽类型）。
//!
//! 交互语义：槽位 = 键/值/数组项的线性序，Enter/Tab 提交并前进、Shift+Tab 后退、Esc 取消；
//! 添加属性两段式（键名 → 值，键名带常用键建议）；徽章键（tags 类）恒存字符串数组；
//! 所有改动经 `stringify` 拼回全文，走与正文同一保存链与撤销栈。

use atelyx_core::frontmatter::{self, PropType, Yaml};
use iced::widget::{button, column, container, operation, row, space, text, text_input, Column, Id};
use iced::{border, Color, Element, Length, Padding, Point, Task};
use std::time::Instant;

use crate::layout_view::PopupState;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::controls;
use crate::ui::notify::Level as NoticeLevel;
use crate::{App, Message, NoteMode, AUTOSAVE_DEBOUNCE};
use iced::widget::text_editor;

/// 徽章键（值渲染为 `#` 胶囊流，恒存字符串数组）。
pub const BADGE_KEYS: &[&str] = &["tags", "aliases", "cssclasses"];

/// 常用键名建议（聚焦即展示、键入子串过滤、排除已有）。
const COMMON_KEYS: &[&str] = &[
    "tags", "aliases", "cssclasses", "status", "priority", "type", "author", "source", "url",
    "created", "updated", "summary",
];

pub fn is_badge_key(key: &str) -> bool {
    BADGE_KEYS.contains(&key)
}

/// 编辑槽（属性面板内的可编辑单元）。
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    /// 属性行序号 → 键名。
    Key(usize),
    /// 属性行序号 → 单值（非数组）。
    Value(usize),
    /// 属性行序号 + 数组项序号。
    Item(usize, usize),
}

/// 添加表单两段式步骤。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AddStep {
    Key,
    Value,
}

#[derive(Clone, Debug)]
pub struct AddForm {
    pub key: String,
    pub step: AddStep,
}

/// 属性面板交互状态（渲染数据缓存在 `parsed`，随文件载入 / 属性提交 / 撤销重做重建）。
pub struct PropsPanel {
    pub collapsed: bool,
    /// 当前编辑槽（None = 无槽编辑）。
    pub editing: Option<Slot>,
    /// 编辑槽 / 添加表单的草稿文本。
    pub draft: String,
    /// 添加表单（None = 关闭）。
    pub adding: Option<AddForm>,
    /// 重复键提示文案。
    pub duplicate: Option<String>,
    /// 缓存的 frontmatter 解析产物：Some((data, ok)) = 有开头块；None = 无块。
    pub parsed: Option<(Yaml, bool)>,
    /// 当前编辑输入框的焦点 id（同时至多一个输入框挂载，共用一个 id）。
    focus_id: Id,
}

impl Default for PropsPanel {
    fn default() -> Self {
        Self {
            collapsed: false,
            editing: None,
            draft: String::new(),
            adding: None,
            duplicate: None,
            parsed: None,
            focus_id: Id::unique(),
        }
    }
}

/// 属性面板消息。
#[derive(Clone, Debug)]
pub enum PropsMsg {
    ToggleCollapse,
    StartEdit(Slot),
    Draft(String),
    /// 提交当前槽并移动：0 = 仅提交；1 / -1 = 前进 / 后退（前进越界接续添加表单）。
    CommitMove(i32),
    CancelSlot,
    DeleteProp(usize),
    ToggleBool(usize),
    AddItem(usize),
    StartAdd,
    /// 添加表单内 Enter：键名步进值步；值步落盘。
    AddCommit,
    AddCancel,
    KeySuggestPick(String),
    /// tags 候选点击：以逗号追加进值输入，不直接落盘。
    TagSuggestPick(String),
    ConvertType(usize, PropType),
    OpenSourceMode,
}

impl PropsPanel {
    pub fn focus_id(&self) -> Id {
        self.focus_id.clone()
    }
}

/// 属性面板渲染：Some(element) = 应显示面板。
/// `standalone` = inspector 视图形态（不随源码档隐藏）；note 视图内源码档不渲染。
pub fn props_panel<'a>(
    app: &'a App,
    window: iced::window::Id,
    standalone: bool,
) -> Option<Element<'a, Message>> {
    if app.ui.last_note_file.is_none() || (!standalone && app.note_mode == NoteMode::Source) {
        return None;
    }
    let parsed = app.props.parsed.as_ref();
    // 无块（无 frontmatter）时整块不显示，但菜单「添加笔记属性」打开的表单要有着落：
    // 添加表单开着才渲染（提交经空数据起块）
    if parsed.is_none() && app.props.adding.is_none() {
        return None;
    }
    let cursor = app.cursor_for(window);
    let body: Element<'a, Message> = match parsed {
        // YAML 解析失败：红条 + 源码模式入口（面板编辑全部禁用，防 stringify 丢弃旧数据）
        Some((_data, false)) => row![
            icon("triangle-alert", 13.0, pal::DANGER),
            text("YAML 格式错误，属性面板暂不可编辑")
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
            space(),
            controls::btn(
                "打开源码模式",
                &btn_tiny(),
                controls::Variant::Secondary,
                Some(Message::Props(PropsMsg::OpenSourceMode)),
            ),
        ]
        .spacing(8)
        .padding(Padding::ZERO.horizontal(16.0).vertical(6.0))
        .into(),
        Some((data, true)) => {
            let Yaml::Map(pairs) = data else { return None };
            props_column(app, window, cursor, pairs).into()
        }
        // 无块：无属性行，仅添加表单
        None => props_column(app, window, cursor, &[]).into(),
    };
    Some(column![body, controls::h_divider_subtle()].into())
}

/// 属性列（折叠头 + 属性行 + 添加表单）；`pairs` = 解析出的键值对（无块传空）。
fn props_column<'a>(
    app: &'a App,
    window: iced::window::Id,
    cursor: Point,
    pairs: &'a [(String, Yaml)],
) -> Column<'a, Message> {
    let mut col = Column::<Message>::new()
        .padding(Padding::ZERO.horizontal(16.0).vertical(6.0))
        .spacing(2);
    if !pairs.is_empty() {
        let chevron = if app.props.collapsed { "chevron-right" } else { "chevron-down" };
        col = col.push(
            button(
                row![
                    icon(chevron, 12.0, pal::TEXT_MUTED),
                    text(format!("笔记属性 ({})", pairs.len()))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_SECONDARY),
                ]
                .spacing(6),
            )
            .padding(2.0)
            .style(|_t, _s| plain_btn())
            .on_press(Message::Props(PropsMsg::ToggleCollapse)),
        );
    }
    if !app.props.collapsed {
        for (i, (key, value)) in pairs.iter().enumerate() {
            col = col.push(prop_row(app, window, cursor, i, key, value));
        }
        col = col.push(add_area(app));
    }
    col
}

/// 语义按钮的透明底样式（视觉由内容承载）。
fn plain_btn() -> button::Style {
    button::Style { background: None, ..button::Style::default() }
}

fn btn_tiny() -> controls::TextSize {
    controls::TextSize { h: 18.0, pad_x: 6.0, font: pal::fs_micro(), radius: pal::RADIUS_XS }
}

/// 单个属性行：类型图标（点击弹类型菜单）+ 键名 + 值区 + 删除。
fn prop_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    cursor: Point,
    index: usize,
    key: &str,
    value: &'a Yaml,
) -> Element<'a, Message> {
    let badge = is_badge_key(key);
    let ptype = frontmatter::infer_type(badge, value);

    let type_btn = button(
        container(icon(type_icon(ptype), 13.0, pal::TEXT_MUTED))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fixed(20.0))
    .height(Length::Fixed(20.0))
    .padding(0.0)
    .style(|_t, _s| plain_btn())
    .on_press(open_type_menu(window, cursor, index));

    let key_cell: Element<'a, Message> = if app.props.editing.as_ref() == Some(&Slot::Key(index)) {
        slot_input(app, "键名", PropsMsg::CommitMove(1)).into()
    } else {
        button(
            container(text(key.to_string()).size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .align_y(iced::alignment::Vertical::Center),
        )
        .width(Length::Fixed(96.0))
        .padding(Padding::ZERO.horizontal(4.0).vertical(3.0))
        .style(|_t, _s| plain_btn())
        .on_press(Message::Props(PropsMsg::StartEdit(Slot::Key(index))))
        .into()
    };

    let value_cell: Element<'a, Message> = match app.props.editing.as_ref() {
        Some(Slot::Value(i)) if *i == index => slot_input(app, "属性值", PropsMsg::CommitMove(1)).into(),
        Some(Slot::Item(pi, _ii)) if *pi == index => slot_input(app, "项", PropsMsg::CommitMove(1)).into(),
        _ => value_display(value, ptype, index),
    };

    let del = button(
        container(icon("x", 11.0, pal::TEXT_MUTED))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fixed(18.0))
    .height(Length::Fixed(18.0))
    .padding(0.0)
    .style(|_t, status| button::Style {
        text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            pal::DANGER
        } else {
            pal::TEXT_MUTED
        },
        ..plain_btn()
    })
    .on_press(Message::Props(PropsMsg::DeleteProp(index)));

    row![type_btn, key_cell, value_cell, del].spacing(6).width(Length::Fill).into()
}

/// 值区展示与编辑入口（按类型分形态：布尔开关 / 徽章流 / 列表 / 可编辑文本 / 只读展示）。
fn value_display<'a>(value: &'a Yaml, ptype: PropType, index: usize) -> Element<'a, Message> {
    match value {
        Yaml::Bool(b) => controls::btn(
            if *b { "true" } else { "false" },
            &btn_tiny(),
            controls::Variant::Secondary,
            Some(Message::Props(PropsMsg::ToggleBool(index))),
        )
        .into(),
        Yaml::Seq(items) if ptype == PropType::List => {
            let mut list = Column::<Message>::new().spacing(0);
            for (j, item) in items.iter().enumerate() {
                list = list.push(
                    row![
                        icon("clock", 11.0, pal::TEXT_MUTED),
                        button(text(item_text(item)).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
                            .padding(Padding::ZERO.horizontal(4.0).vertical(1.0))
                            .style(|_t, _s| plain_btn())
                            .on_press(Message::Props(PropsMsg::StartEdit(Slot::Item(index, j)))),
                    ]
                    .spacing(4),
                );
            }
            list = list.push(
                button(text("添加").size(pal::fs_micro()).color(pal::TEXT_MUTED))
                    .padding(Padding::ZERO.horizontal(4.0).vertical(1.0))
                    .style(|_t, _s| plain_btn())
                    .on_press(Message::Props(PropsMsg::AddItem(index))),
            );
            list.into()
        }
        Yaml::Seq(items) => {
            let mut flow = row!().spacing(4);
            for (j, item) in items.iter().enumerate() {
                flow = flow.push(
                    button(
                        container(text(format!("#{}", item_text(item))).size(pal::fs_micro()).color(pal::ACCENT))
                            .padding(Padding::ZERO.horizontal(6.0).vertical(2.0)),
                    )
                    .padding(0.0)
                    .style(|_t, _s| button::Style {
                        background: Some(pal::ACCENT_SOFT.into()),
                        border: border::rounded(9.0),
                        ..button::Style::default()
                    })
                    .on_press(Message::Props(PropsMsg::StartEdit(Slot::Item(index, j)))),
                );
            }
            flow = flow.push(
                button(container(icon("plus", 11.0, pal::TEXT_MUTED)))
                    .width(Length::Fixed(18.0))
                    .height(Length::Fixed(18.0))
                    .padding(0.0)
                    .style(|_t, status| button::Style {
                        text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                            pal::TEXT_PRIMARY
                        } else {
                            pal::TEXT_MUTED
                        },
                        ..plain_btn()
                    })
                    .on_press(Message::Props(PropsMsg::AddItem(index))),
            );
            flow.into()
        }
        Yaml::Str(s) => {
            let shown = if s.is_empty() { "（空）".to_string() } else { s.clone() };
            button(
                container(
                    text(shown)
                        .size(pal::fs_caption())
                        .color(if s.is_empty() { pal::TEXT_MUTED } else { pal::TEXT_PRIMARY }),
                )
                .width(Length::Fill)
                .align_y(iced::alignment::Vertical::Center),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(4.0).vertical(3.0))
            .style(|_t, _s| plain_btn())
            .on_press(Message::Props(PropsMsg::StartEdit(Slot::Value(index))))
            .into()
        }
        other => text(item_text(other)).size(pal::fs_caption()).color(pal::TEXT_SECONDARY).into(),
    }
}

/// 添加表单（两段式 + 键名建议内联下拉）与面板底部「添加属性」按钮。
fn add_area<'a>(app: &'a App) -> Element<'a, Message> {
    let Some(form) = &app.props.adding else {
        return button(
            row![
                icon("plus", 11.0, pal::TEXT_MUTED),
                text("添加属性").size(pal::fs_micro()).color(pal::TEXT_MUTED),
            ]
            .spacing(4),
        )
        .padding(Padding::ZERO.horizontal(4.0).vertical(3.0))
        .style(|_t, _s| plain_btn())
        .on_press(Message::Props(PropsMsg::StartAdd))
        .into();
    };
    let mut col = Column::<Message>::new().spacing(2);
    match form.step {
        AddStep::Key => {
            col = col.push(slot_input(app, "属性名", PropsMsg::AddCommit));
            for name in key_suggestions(app, &app.props.draft) {
                col = col.push(
                    button(text(name.to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY))
                        .width(Length::Fill)
                        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                        .style(|_t, status| button::Style {
                            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                                .then_some(pal::ACCENT_SOFT.into()),
                            ..plain_btn()
                        })
                        .on_press(Message::Props(PropsMsg::KeySuggestPick(name.to_string()))),
                );
            }
        }
        AddStep::Value => {
            col = col.push(
                row![
                    container(text(format!("#{}", form.key)).size(pal::fs_micro()).color(pal::ACCENT))
                        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                        .style(|_t| iced::widget::container::Style {
                            background: Some(pal::ACCENT_SOFT.into()),
                            border: border::rounded(9.0),
                            ..iced::widget::container::Style::default()
                        }),
                    slot_input(app, "属性值（逗号分隔为数组）", PropsMsg::AddCommit),
                ]
                .spacing(6),
            );
            // 徽章键 = 全仓库标签候选（子串过滤、排除本笔记已有，上限 8）
            if is_badge_key(&form.key) {
                for tag in tag_suggestions(app, &app.props.draft) {
                    col = col.push(
                        button(
                            row![
                                icon("tag", 11.0, pal::TEXT_MUTED),
                                text(format!("#{tag}")).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                            ]
                            .spacing(4),
                        )
                        .width(Length::Fill)
                        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                        .style(|_t, status| button::Style {
                            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                                .then_some(pal::ACCENT_SOFT.into()),
                            ..plain_btn()
                        })
                        .on_press(Message::Props(PropsMsg::TagSuggestPick(tag))),
                    );
                }
            }
        }
    }
    if let Some(dup) = &app.props.duplicate {
        col = col.push(text(dup.clone()).size(pal::fs_micro()).color(pal::DANGER));
    }
    col.into()
}

/// 键名建议：常用键按子串过滤（不区分大小写）、排除已有，上限 8 条。
fn key_suggestions(app: &App, query: &str) -> Vec<&'static str> {
    let existing: Vec<String> = match &app.props.parsed {
        Some((Yaml::Map(pairs), true)) => pairs.iter().map(|(k, _)| k.clone()).collect(),
        _ => Vec::new(),
    };
    let q = query.trim().to_ascii_lowercase();
    COMMON_KEYS
        .iter()
        .filter(|k| !existing.iter().any(|e| e == **k))
        .filter(|k| q.is_empty() || k.to_ascii_lowercase().contains(&q))
        .copied()
        .take(8)
        .collect()
}

/// tags 候选：全仓库词汇表按子串过滤（不区分大小写）、排除本笔记徽章键（tags/aliases/cssclasses）值并集，上限 8 条。
fn tag_suggestions(app: &App, query: &str) -> Vec<String> {
    let existing: Vec<String> = match &app.props.parsed {
        Some((Yaml::Map(pairs), true)) => pairs
            .iter()
            .filter(|(k, _)| is_badge_key(k))
            .flat_map(|(_, v)| match v {
                Yaml::Seq(items) => items.iter().map(item_text).collect::<Vec<_>>(),
                other => vec![item_text(other)],
            })
            .collect(),
        _ => Vec::new(),
    };
    // 只按最后一段过滤（值输入是逗号分隔的多段）
    let q = query.trim().to_lowercase();
    let last = q.rsplit(['，', ',']).next().unwrap_or("").trim().to_string();
    app.tag_vocab
        .iter()
        .filter(|row| !existing.iter().any(|e| e == &row.tag))
        .filter(|row| last.is_empty() || row.tag.to_lowercase().contains(&last))
        .take(8)
        .map(|row| row.tag.clone())
        .collect()
}

/// 槽位输入框（透明底、聚焦描边；输入统一写 draft，Enter 提交动机由调用方给定）。
fn slot_input<'a>(app: &'a App, placeholder: &str, submit: PropsMsg) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, &app.props.draft)
        .size(pal::fs_caption())
        .padding(Padding::ZERO.horizontal(4.0).vertical(2.0))
        .width(Length::Fill)
        .id(app.props.focus_id.clone())
        .on_input(|s| Message::Props(PropsMsg::Draft(s)))
        .on_submit(Message::Props(submit))
        .style(|_theme, status| text_input::Style {
            background: Color::TRANSPARENT.into(),
            border: border::rounded(pal::RADIUS_XS)
                .color(pal::ACCENT)
                .width(if matches!(status, text_input::Status::Focused { .. }) { 1.0 } else { 0.0 }),
            icon: pal::TEXT_MUTED,
            placeholder: pal::INPUT_PLACEHOLDER,
            value: pal::TEXT_PRIMARY,
            selection: pal::SELECTION,
        })
}

fn type_icon(t: PropType) -> &'static str {
    match t {
        PropType::Text => "type",
        PropType::Tag => "tag",
        PropType::Date => "calendar-days",
        PropType::Number => "hash",
        PropType::Boolean => "toggle-left",
        PropType::List => "list",
    }
}

fn item_text(v: &Yaml) -> String {
    match v {
        Yaml::Null => String::new(),
        Yaml::Date(d) => d.clone(),
        Yaml::Num(f) => display_num(*f),
        Yaml::Bool(b) => if *b { "true" } else { "false" }.into(),
        Yaml::Str(s) => s.clone(),
        Yaml::Seq(_) | Yaml::Map(_) => "[object Object]".into(),
    }
}

/// 显示用数字文本（整数不带小数点；指数形态交由 core 序列化处理）。
fn display_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e21 && v.abs() >= 1e-6 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn open_type_menu(window: iced::window::Id, cursor: Point, index: usize) -> Message {
    Message::PopupOpen(
        window,
        crate::layout_view::PopupOpen {
            point: Point::new(cursor.x, cursor.y + 14.0),
            flip_y: Some(cursor.y - 12.0),
            state: PopupState::PropTypeMenu { index },
            min_width: 0.0,
        },
    )
}

// ===== 交互处理 =====

impl App {
    /// 聚焦当前槽输入框并全选（进入编辑即全选）。
    fn focus_slot(&self) -> Task<Message> {
        let id = self.props.focus_id.clone();
        operation::focus(id.clone()).chain(operation::select_all(id))
    }

    pub(crate) fn on_props(&mut self, m: PropsMsg) -> Task<Message> {
        match m {
            PropsMsg::ToggleCollapse => {
                self.props.collapsed = !self.props.collapsed;
                Task::none()
            }
            PropsMsg::OpenSourceMode => {
                self.props.editing = None;
                self.props.adding = None;
                self.handle(Message::NoteMode(NoteMode::Source))
            }
            PropsMsg::StartEdit(slot) => {
                // 编辑槽与添加表单互斥（共用一个输入框 id，不得同时挂载）
                self.props.adding = None;
                self.props.editing = Some(slot);
                self.props.draft = self.slot_text();
                self.props.duplicate = None;
                self.focus_slot()
            }
            PropsMsg::Draft(s) => {
                if self.props.editing.is_some() || self.props.adding.is_some() {
                    self.props.draft = s;
                }
                Task::none()
            }
            PropsMsg::CancelSlot => {
                self.props.editing = None;
                self.props.adding = None;
                self.props.draft.clear();
                Task::none()
            }
            PropsMsg::CommitMove(dir) => self.commit_slot(dir),
            PropsMsg::DeleteProp(i) => {
                if let Some(mut data) = self.props_data() {
                    if i < data.len() {
                        data.remove(i);
                        self.apply_props(data);
                    }
                }
                Task::none()
            }
            PropsMsg::ToggleBool(i) => {
                if let Some(mut data) = self.props_data() {
                    if let Some((_, Yaml::Bool(b))) = data.get_mut(i) {
                        *b = !*b;
                        self.apply_props(data);
                    }
                }
                Task::none()
            }
            PropsMsg::AddItem(pi) => {
                if let Some(mut data) = self.props_data() {
                    if let Some((_, Yaml::Seq(items))) = data.get_mut(pi) {
                        items.push(Yaml::Str(String::new()));
                        let j = items.len() - 1;
                        self.apply_props(data);
                        self.props.editing = Some(Slot::Item(pi, j));
                        self.props.draft.clear();
                        return self.focus_slot();
                    }
                }
                Task::none()
            }
            PropsMsg::StartAdd => {
                // 源码档：无 frontmatter 块时在文首插入空骨架（已有块静默不动，不切模式）
                if self.note_mode == NoteMode::Source {
                    if frontmatter::block_end(&self.note_text).is_none() {
                        self.edit.snapshot(&self.note_text);
                        let mut next = String::from("---\n\n---\n\n");
                        next.push_str(&self.note_text);
                        self.note_text = next;
                        self.source_editor = text_editor::Content::with_text(&self.note_text);
                        self.dirty = true;
                        self.save_error = None;
                        self.autosave_deadline = Some(Instant::now() + AUTOSAVE_DEBOUNCE);
                        self.notify(NoticeLevel::Info, "已在文首插入属性块");
                    }
                    return Task::none();
                }
                self.props.editing = None;
                self.props.adding = Some(AddForm { key: String::new(), step: AddStep::Key });
                self.props.collapsed = false;
                self.props.draft.clear();
                self.props.duplicate = None;
                self.focus_slot()
            }
            PropsMsg::KeySuggestPick(name) => {
                if let Some(form) = self.props.adding.as_mut() {
                    form.key = name;
                    form.step = AddStep::Value;
                }
                self.props.draft.clear();
                self.props.duplicate = None;
                self.focus_slot()
            }
            PropsMsg::TagSuggestPick(tag) => {
                // 候选点击 = 追加进值输入（中文逗号分隔），由 Enter 统一落盘
                let trimmed = self.props.draft.trim().to_string();
                self.props.draft = if trimmed.is_empty() {
                    tag
                } else {
                    format!("{trimmed}，{tag}")
                };
                self.focus_slot()
            }
            PropsMsg::AddCommit => self.add_commit(),
            PropsMsg::AddCancel => {
                self.props.adding = None;
                self.props.draft.clear();
                self.props.duplicate = None;
                Task::none()
            }
            PropsMsg::ConvertType(i, target) => {
                if let Some(mut data) = self.props_data() {
                    if let Some((_, v)) = data.get_mut(i) {
                        if let Some(converted) = frontmatter::convert_value(v, target) {
                            *v = converted;
                            self.apply_props(data);
                        }
                    }
                }
                Task::none()
            }
        }
    }

    /// 当前属性数据（无块 / 解析失败 / 非映射 → None）。
    fn props_data(&self) -> Option<Vec<(String, Yaml)>> {
        match &self.props.parsed {
            Some((Yaml::Map(pairs), true)) => Some(pairs.clone()),
            _ => None,
        }
    }

    /// 当前编辑槽的现值（进入编辑时的草稿初值）。
    fn slot_text(&self) -> String {
        let Some(data) = self.props_data() else { return String::new() };
        let text = match &self.props.editing {
            Some(Slot::Key(i)) => data.get(*i).map(|(k, _)| k.clone()),
            Some(Slot::Value(i)) => data.get(*i).and_then(|(_, v)| scalar_text(v)),
            Some(Slot::Item(pi, ii)) => data.get(*pi).and_then(|(_, v)| match v {
                Yaml::Seq(items) => items.get(*ii).and_then(scalar_text),
                _ => None,
            }),
            _ => None,
        };
        text.unwrap_or_default()
    }

    /// 提交当前编辑槽并按方向移动（槽被删空则不前进；前进越界接续添加表单）。
    fn commit_slot(&mut self, dir: i32) -> Task<Message> {
        let Some(slot) = self.props.editing.clone() else { return Task::none() };
        let Some(mut data) = self.props_data() else {
            self.props.editing = None;
            return Task::none();
        };
        // 槽越界守卫（undo 重建的属性数据可能比编辑发起时少项）：越界 = 取消编辑不提交
        let valid = match &slot {
            Slot::Key(i) | Slot::Value(i) => *i < data.len(),
            Slot::Item(pi, ii) => data.get(*pi).is_some_and(|(_, v)| {
                matches!(v, Yaml::Seq(items) if *ii < items.len())
            }),
        };
        if !valid {
            self.props.editing = None;
            return Task::none();
        }
        let draft = std::mem::take(&mut self.props.draft);
        let (changed, deleted) = commit_slot_data(&mut data, &slot, &draft);
        if changed {
            self.apply_props(data.clone());
        }
        self.props.editing = None;
        if deleted || dir == 0 {
            return Task::none();
        }
        let slots = flatten_slots(&data);
        let target = match slots.iter().position(|s| *s == slot) {
            Some(p) if dir > 0 => slots.get(p + 1).cloned(),
            Some(p) => p.checked_sub(1).and_then(|q| slots.get(q).cloned()),
            None => None,
        };
        match target {
            Some(next) => {
                self.props.editing = Some(next);
                self.props.draft = self.slot_text();
                self.focus_slot()
            }
            None if dir > 0 => self.on_props(PropsMsg::StartAdd),
            None => Task::none(),
        }
    }

    /// 添加表单 Enter：键名步进值步；值步落盘（徽章键恒数组、含逗号拆数组、重复键回键名步）。
    /// 输入缓冲统一用 `draft`（键名步 = 键名草稿，值步 = 值草稿）。
    fn add_commit(&mut self) -> Task<Message> {
        let Some(form) = self.props.adding.clone() else { return Task::none() };
        match form.step {
            AddStep::Key => {
                let name = self.props.draft.trim().to_string();
                if name.is_empty() {
                    return self.on_props(PropsMsg::AddCancel);
                }
                let dup = self.props_data().is_some_and(|d| d.iter().any(|(k, _)| k == &name));
                if dup {
                    self.props.duplicate = Some(format!("属性「{name}」已存在，请改键名或编辑该属性"));
                    return self.focus_slot();
                }
                if let Some(f) = self.props.adding.as_mut() {
                    f.key = name;
                    f.step = AddStep::Value;
                }
                self.props.draft.clear();
                self.focus_slot()
            }
            AddStep::Value => {
                let name = form.key.clone();
                let value = self.props.draft.trim().to_string();
                if value.is_empty() {
                    return self.on_props(PropsMsg::AddCancel);
                }
                let badge = is_badge_key(&name);
                let mut data = self.props_data().unwrap_or_default();
                let items: Vec<String> = split_comma_list(&value);
                match data.iter().position(|(k, _)| k == &name) {
                    Some(i) if badge && matches!(data[i].1, Yaml::Seq(_)) => {
                        // 徽章键且原值是数组 = 直接追加进数组
                        if let Yaml::Seq(list) = &mut data[i].1 {
                            list.extend(items.into_iter().map(Yaml::Str));
                        }
                    }
                    Some(_) => {
                        self.props.duplicate = Some(format!("属性「{name}」已存在，请改键名或编辑该属性"));
                        if let Some(f) = self.props.adding.as_mut() {
                            f.step = AddStep::Key;
                            self.props.draft = f.key.clone();
                        }
                        return self.focus_slot();
                    }
                    None => {
                        let val = if badge || items.len() > 1 {
                            Yaml::Seq(items.into_iter().map(Yaml::Str).collect())
                        } else {
                            Yaml::Str(value)
                        };
                        data.push((name, val));
                    }
                }
                self.props.adding = None;
                self.props.duplicate = None;
                self.props.draft.clear();
                self.apply_props(data);
                Task::none()
            }
        }
    }

    /// 用新属性数据重写 frontmatter 并走正文保存链（空映射 = 删块回到无 frontmatter 态）。
    fn apply_props(&mut self, data: Vec<(String, Yaml)>) {
        self.edit.snapshot(&self.note_text);
        let body = crate::strip_frontmatter(&self.note_text).to_string();
        self.note_text = frontmatter::stringify(&Yaml::Map(data), &body);
        self.dirty = true;
        self.save_error = None;
        self.autosave_deadline = Some(Instant::now() + crate::AUTOSAVE_DEBOUNCE);
        self.refresh_props();
    }

    /// 重建属性面板解析缓存（文件载入 / 属性提交 / 撤销重做后调用）。
    /// 未闭合块（首行 `---` 但无闭合行）：按解析失败处理（红条锁编辑）——其 data 已把
    /// 正文吞入 YAML 区域，直接 stringify 会把正文复制进属性块。
    pub fn refresh_props(&mut self) {
        let parsed = frontmatter::parse(&self.note_text);
        let open = frontmatter::block_end(&self.note_text).is_some();
        self.props.parsed = if open {
            Some((parsed.data, parsed.ok))
        } else if self.note_text.starts_with("---") {
            Some((Yaml::Map(Vec::new()), false))
        } else {
            None
        };
    }

    /// 清空属性面板交互态（切文件 / 关文件 / 切仓库时）。
    pub fn reset_props(&mut self) {
        self.props.editing = None;
        self.props.adding = None;
        self.props.duplicate = None;
        self.props.draft.clear();
        self.refresh_props();
    }
}

/// 槽位提交：返回（数据是否有变化、槽是否被删空）。
/// 键名 trim 空 = 删属性；改名撞已有键或未变 = 无变化（保守退出）；
/// 单值 trim 空 = 删属性；数组项 trim 空 = 删项；数组项含逗号 = 拆多项替换。
fn commit_slot_data(data: &mut Vec<(String, Yaml)>, slot: &Slot, draft: &str) -> (bool, bool) {
    let value = draft.trim();
    match slot {
        Slot::Key(i) => {
            let name = value.to_string();
            if name.is_empty() {
                data.remove(*i);
                (true, true)
            } else if name == data[*i].0 || data.iter().enumerate().any(|(j, (k, _))| j != *i && k == &name) {
                (false, false)
            } else {
                data[*i].0 = name;
                (true, false)
            }
        }
        Slot::Value(i) => {
            if value.is_empty() {
                data.remove(*i);
                (true, true)
            } else {
                data[*i].1 = Yaml::Str(value.to_string());
                (true, false)
            }
        }
        Slot::Item(pi, ii) => {
            let Yaml::Seq(items) = &mut data[*pi].1 else { return (false, false) };
            if value.is_empty() {
                items.remove(*ii);
                (true, true)
            } else {
                let parts: Vec<String> = split_comma_list(value);
                if parts.len() == 1 {
                    items[*ii] = Yaml::Str(parts.into_iter().next().unwrap());
                } else {
                    items.splice(*ii..=*ii, parts.into_iter().map(Yaml::Str));
                }
                (true, false)
            }
        }
    }
}

/// 槽位线性序（键 → 值 / 数组项）。
fn flatten_slots(data: &[(String, Yaml)]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (i, (_, v)) in data.iter().enumerate() {
        slots.push(Slot::Key(i));
        match v {
            Yaml::Seq(items) => {
                for j in 0..items.len() {
                    slots.push(Slot::Item(i, j));
                }
            }
            _ => slots.push(Slot::Value(i)),
        }
    }
    slots
}

fn scalar_text(v: &Yaml) -> Option<String> {
    match v {
        Yaml::Str(s) => Some(s.clone()),
        Yaml::Date(d) => Some(d.clone()),
        Yaml::Num(f) => Some(display_num(*f)),
        _ => None,
    }
}

/// 中英文逗号拆分（添加表单与数组项的数组展开规则）。
fn split_comma_list(s: &str) -> Vec<String> {
    s.split([',', '，'])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}
