//! 笔记属性面板：frontmatter 的类型化渲染与槽位编辑（数据源 = core frontmatter 宽类型）。
//!
//! 改动经 `stringify` 拼回全文，与正文同一条保存链；交互语义见 `PropsMsg` 与 `commit_slot_data`。

use atelyx_core::frontmatter::{self, PropType, Yaml};
use iced::widget::{button, column, container, mouse_area, operation, row, scrollable, space, text_input, Column, Id};
use iced::{border, Color, Element, Length, Padding, Point, Task};
use std::time::Instant;

use crate::layout_view::{estimate_text_width, PopupState};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::text;
use crate::{App, Message, NoteMode};
use iced::widget::text_editor;

/// 徽章键（值渲染为 `#` 胶囊流，恒存字符串数组）：仅 tags；aliases / cssclasses 是普通列表。
pub const BADGE_KEYS: &[&str] = &["tags"];

/// 常用键名建议（聚焦即展示、键入子串过滤、排除已有）。
const COMMON_KEYS: &[&str] = &[
    "tags", "aliases", "cssclasses", "status", "priority", "type", "author", "source", "url",
    "created", "updated", "summary",
];

/// 候选清单可视行数上限：命中再多也只在卡内滚动，卡片不把以下属性行顶出视野。
const SUGGEST_MAX_ROWS: usize = 8;

/// 候选行高：整行可点，悬停底与键盘高亮都按这个矩形表达；随界面字号放大（大字号下不裁行）。
fn suggest_row_h() -> f32 {
    (pal::fs_caption() * 2.0).max(24.0)
}

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
    /// 数组行尚未写入的新项（提交非空才追加；空提交 = 取消）。
    NewItem(usize),
}

/// 行内动作的行身份：渲染时的键名 + （数组项）下标与文本。
/// 提交式失焦可能删行 / 拆项使下标漂移，动作先按身份重定位再执行，找不到即放弃。
#[derive(Clone, Debug, PartialEq)]
pub struct RowRef {
    pub key: String,
    pub item: Option<(usize, String)>,
}

impl RowRef {
    fn row(key: &str) -> Self {
        Self { key: key.to_string(), item: None }
    }

    fn item(key: &str, index: usize, value: &Yaml) -> Self {
        Self { key: key.to_string(), item: Some((index, item_text(value))) }
    }
}

/// 候选列表状态：query 进入编辑时为空（展示候选池顶部），键入才过滤；
/// armed = 按过 ↑↓（仅此时 Enter 选中候选，否则提交手输文本）。
#[derive(Clone, Debug, Default)]
pub struct Suggest {
    pub index: usize,
    pub armed: bool,
    pub query: String,
}

/// 一份候选清单：`pool` = 过滤前的池大小。
/// 池非空而命中为空时留一行提示（清单高度不随键入忽隐忽现），池为空则整卡不出现。
#[derive(Clone, Debug, Default)]
struct SuggestList {
    items: Vec<String>,
    pool: usize,
}

impl SuggestList {
    fn hidden(&self) -> bool {
        self.pool == 0
    }
}

/// 属性面板交互状态（渲染数据缓存在 `parsed`，随文件载入 / 属性提交 / 撤销重做重建）。
pub struct PropsPanel {
    pub collapsed: bool,
    /// 当前编辑槽（None = 无槽编辑）。
    pub editing: Option<Slot>,
    /// 编辑槽的草稿文本。
    pub draft: String,
    /// 重复键提示文案。
    pub duplicate: Option<String>,
    /// 缓存的 frontmatter 解析产物：Some((data, ok)) = 有开头块；None = 无块。
    pub parsed: Option<(Yaml, bool)>,
    /// 候选高亮与过滤。
    suggest: Suggest,
    /// 悬停行 / 悬停数组项（删除钮显隐；带序号防迟到离场清掉新悬停）。
    hover_row: Option<usize>,
    hover_item: Option<(usize, usize)>,
    /// 同批按压落在属性面板内（PressReporter 上报，`Message::Press` 消费）。
    pub press_inside: bool,
    /// 当前编辑输入框的焦点 id（同时至多一个输入框挂载，共用一个 id）。
    focus_id: Id,
    /// 候选清单滚动区 id（键盘高亮跟随滚动用）。
    suggest_scroll: Id,
}

impl Default for PropsPanel {
    fn default() -> Self {
        Self {
            collapsed: false,
            editing: None,
            draft: String::new(),
            duplicate: None,
            parsed: None,
            suggest: Suggest::default(),
            hover_row: None,
            hover_item: None,
            press_inside: false,
            focus_id: Id::unique(),
            suggest_scroll: Id::unique(),
        }
    }
}

/// 属性面板消息。
#[derive(Clone, Debug)]
pub enum PropsMsg {
    ToggleCollapse,
    /// 进入槽编辑；`row` = 渲染时的行身份（提交式失焦可能删行，动作按键名 / 项文本重定位）。
    StartEdit(Slot, RowRef),
    Draft(String),
    /// Enter：候选已用 ↑↓ 高亮则选中候选，否则提交手输文本。
    Submit,
    /// Tab / Shift+Tab：提交并前进 / 后退（不选候选）。
    CommitMove(i32),
    CancelSlot,
    DeleteProp(RowRef),
    /// 删除数组项（行身份含项下标与文本）。
    DeleteItem(RowRef),
    ToggleBool(RowRef),
    AddItem(RowRef),
    /// 追加一条空属性（占位键立即写盘）并进入键名编辑。
    StartAdd,
    /// 候选点击（恒选中）。
    SuggestPick(String),
    /// 候选高亮移动（↑↓）。
    SuggestMove(i32),
    ConvertType(String, PropType),
    OpenTypeMenu {
        key: String,
        window: iced::window::Id,
        cursor: Point,
    },
    OpenSourceMode,
    HoverRow(usize),
    UnhoverRow(usize),
    HoverItem(usize, usize),
    UnhoverItem(usize, usize),
    /// 同批按压落在属性面板内。
    PressInside,
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
    // 无块（无 frontmatter）时整块不显示；菜单「添加笔记属性」会先把占位属性写进文件，块随之出现
    let Some(parsed) = parsed else { return None };
    let cursor = app.cursor_for(window);
    let body: Element<'a, Message> = match parsed {
        // YAML 解析失败：红条 + 源码模式入口（面板编辑全部禁用，防 stringify 丢弃旧数据）
        (_data, false) => row![
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
        (data, true) => {
            let Yaml::Map(pairs) = data else { return None };
            props_column(app, window, cursor, pairs).into()
        }
    };
    // 面板内按压转播：外点失焦提交的「内 / 外」判定源（按钮与输入框会捕获按压，mouse_area 不上报）
    Some(
        controls::PressReporter::new(
            column![body, controls::h_divider_subtle()].into(),
            Message::Props(PropsMsg::PressInside),
        )
        .into(),
    )
}

/// 属性列（折叠头 + 属性行 + 底部添加入口）；`pairs` = 解析出的键值对。
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

/// 单行悬停删除钮（显示即红色；不可见时等宽占位不接事件，避免布局跳动）。
fn hover_delete<'a>(size: f32, visible: bool, msg: Message) -> Element<'a, Message> {
    if !visible {
        return space().width(Length::Fixed(size)).height(Length::Fixed(size)).into();
    }
    button(
        container(icon("x", (size - 4.0).max(8.0), pal::DANGER))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .padding(0.0)
    .style(|_t, _s| plain_btn())
    .on_press(msg)
    .into()
}

/// 单个属性行：类型图标（点击弹类型菜单）+ 键名（悬停删除整行）+ 值区。
fn prop_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    cursor: Point,
    index: usize,
    key: &str,
    value: &'a Yaml,
) -> Element<'a, Message> {
    let ptype = frontmatter::infer_type(is_badge_key(key), value);
    let key_ref = RowRef::row(key);

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
    .on_press(Message::Props(PropsMsg::OpenTypeMenu {
        key: key.to_string(),
        window,
        cursor,
    }));

    let key_cell: Element<'a, Message> = if app.props.editing.as_ref() == Some(&Slot::Key(index)) {
        slot_input(app, "键名", Length::Fill).into()
    } else {
        button(
            container(text(key.to_string()).size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .align_y(iced::alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(4.0).vertical(3.0))
        .style(|_t, _s| plain_btn())
        .on_press(Message::Props(PropsMsg::StartEdit(Slot::Key(index), key_ref.clone())))
        .into()
    };
    // 整行删除钮只在悬停键名列时露出（含预留按钮位，移到按钮上仍保持显示）
    let key_area = mouse_area(
        container(
            row![
                key_cell,
                hover_delete(
                    14.0,
                    app.props.hover_row == Some(index),
                    Message::Props(PropsMsg::DeleteProp(key_ref)),
                ),
            ]
            .spacing(2)
            .align_y(iced::alignment::Vertical::Center),
        )
        .width(Length::Fixed(96.0))
        .align_y(iced::alignment::Vertical::Center),
    )
    .on_enter(Message::Props(PropsMsg::HoverRow(index)))
    .on_exit(Message::Props(PropsMsg::UnhoverRow(index)));

    let value_cell = value_display(app, index, key, value, ptype);
    let header = row![type_btn, key_area, value_cell].spacing(6).width(Length::Fill);

    let mut col = Column::<Message>::new().spacing(2).push(header);
    if let Some((list, tags)) = row_suggestions(app, index) {
        if !list.hidden() {
            col = col.push(suggest_card(app, &list, tags));
        }
    }
    col.into()
}

/// 值区展示与编辑入口（按类型分形态：布尔开关 / 徽章流 / 列表 / 可编辑文本 / 只读展示）。
fn value_display<'a>(
    app: &'a App,
    index: usize,
    key: &str,
    value: &'a Yaml,
    ptype: PropType,
) -> Element<'a, Message> {
    let editing = app.props.editing.as_ref();
    match value {
        Yaml::Bool(b) => controls::btn(
            if *b { "true" } else { "false" },
            &btn_tiny(),
            controls::Variant::Secondary,
            Some(Message::Props(PropsMsg::ToggleBool(RowRef::row(key)))),
        )
        .into(),
        Yaml::Seq(items) if ptype == PropType::List => {
            let mut list = Column::<Message>::new().spacing(0);
            for (j, item) in items.iter().enumerate() {
                let row_ref = RowRef::item(key, j, item);
                let item_cell: Element<'a, Message> = if editing == Some(&Slot::Item(index, j)) {
                    slot_input(app, "项", item_input_width(&app.props.draft)).into()
                } else if scalar_text(item).is_some() {
                    button(text(item_text(item)).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
                        .padding(Padding::ZERO.horizontal(4.0).vertical(1.0))
                        .style(|_t, _s| plain_btn())
                        .on_press(Message::Props(PropsMsg::StartEdit(Slot::Item(index, j), row_ref.clone())))
                        .into()
                } else {
                    text(item_text(item)).size(pal::fs_caption()).color(pal::TEXT_PRIMARY).into()
                };
                let line = row![
                    icon("clock", 11.0, pal::TEXT_MUTED),
                    item_cell,
                    hover_delete(
                        13.0,
                        app.props.hover_item == Some((index, j)),
                        Message::Props(PropsMsg::DeleteItem(row_ref)),
                    ),
                ]
                .spacing(4)
                .align_y(iced::alignment::Vertical::Center);
                list = list.push(
                    mouse_area(line)
                        .on_enter(Message::Props(PropsMsg::HoverItem(index, j)))
                        .on_exit(Message::Props(PropsMsg::UnhoverItem(index, j))),
                );
            }
            if editing == Some(&Slot::NewItem(index)) {
                list = list.push(
                    row![
                        icon("clock", 11.0, pal::TEXT_MUTED),
                        slot_input(app, "项", item_input_width(&app.props.draft)),
                    ]
                    .spacing(4)
                    .align_y(iced::alignment::Vertical::Center),
                );
            } else {
                list = list.push(
                    button(text("添加").size(pal::fs_micro()).color(pal::TEXT_MUTED))
                        .padding(Padding::ZERO.horizontal(4.0).vertical(1.0))
                        .style(|_t, _s| plain_btn())
                        .on_press(Message::Props(PropsMsg::AddItem(RowRef::row(key)))),
                );
            }
            list.into()
        }
        Yaml::Seq(items) => {
            let mut flow = row!().spacing(4).align_y(iced::alignment::Vertical::Center);
            for (j, item) in items.iter().enumerate() {
                let row_ref = RowRef::item(key, j, item);
                if editing == Some(&Slot::Item(index, j)) {
                    flow = flow.push(slot_input(app, "标签", item_input_width(&app.props.draft)));
                    continue;
                }
                let editable = scalar_text(item).is_some();
                let label = text(format!("#{}", item_text(item))).size(pal::fs_micro()).color(pal::ACCENT);
                let text_cell: Element<'a, Message> = if editable {
                    button(label)
                        .padding(Padding::ZERO.horizontal(2.0).vertical(2.0))
                        .style(|_t, _s| plain_btn())
                        .on_press(Message::Props(PropsMsg::StartEdit(Slot::Item(index, j), row_ref.clone())))
                        .into()
                } else {
                    container(label).padding(Padding::ZERO.horizontal(2.0).vertical(2.0)).into()
                };
                let pill = container(
                    row![
                        text_cell,
                        hover_delete(
                            12.0,
                            app.props.hover_item == Some((index, j)),
                            Message::Props(PropsMsg::DeleteItem(row_ref)),
                        ),
                    ]
                    .spacing(1)
                    .align_y(iced::alignment::Vertical::Center),
                )
                .padding(Padding::ZERO.horizontal(5.0).vertical(2.0))
                .style(|_t| iced::widget::container::Style {
                    background: Some(pal::ACCENT_SOFT.into()),
                    border: border::rounded(9.0),
                    ..iced::widget::container::Style::default()
                });
                flow = flow.push(
                    mouse_area(pill)
                        .on_enter(Message::Props(PropsMsg::HoverItem(index, j)))
                        .on_exit(Message::Props(PropsMsg::UnhoverItem(index, j))),
                );
            }
            if editing == Some(&Slot::NewItem(index)) {
                flow = flow.push(slot_input(app, "标签", item_input_width(&app.props.draft)));
            } else {
                flow = flow.push(
                    button(
                        container(icon("plus", 11.0, pal::TEXT_MUTED))
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
                            pal::TEXT_PRIMARY
                        } else {
                            pal::TEXT_MUTED
                        },
                        ..plain_btn()
                    })
                    .on_press(Message::Props(PropsMsg::AddItem(RowRef::row(key)))),
                );
            }
            flow.into()
        }
        Yaml::Str(s) => {
            if editing == Some(&Slot::Value(index)) {
                slot_input(app, "属性值", Length::Fill).into()
            } else {
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
                .on_press(Message::Props(PropsMsg::StartEdit(Slot::Value(index), RowRef::row(key))))
                .into()
            }
        }
        other => text(item_text(other)).size(pal::fs_caption()).color(pal::TEXT_SECONDARY).into(),
    }
}

/// 面板底部「添加属性」入口：常驻；有槽正在编辑时不可再点（免得同时多出第二条空属性）。
fn add_area<'a>(app: &'a App) -> Element<'a, Message> {
    let idle = app.props.editing.is_none();
    let mut col = Column::<Message>::new().spacing(2);
    col = col.push(
        button(
            row![
                icon("plus", 11.0, pal::TEXT_MUTED),
                text("添加属性")
                    .size(pal::fs_micro())
                    .color(if idle { pal::TEXT_MUTED } else { pal::TEXT_MUTED.scale_alpha(0.5) }),
            ]
            .spacing(4)
            .align_y(iced::alignment::Vertical::Center),
        )
        .padding(Padding::ZERO.horizontal(4.0).vertical(3.0))
        .style(|_t, _s| plain_btn())
        .on_press_maybe(idle.then(|| Message::Props(PropsMsg::StartAdd))),
    );
    if let Some(dup) = &app.props.duplicate {
        col = col.push(text(dup.clone()).size(pal::fs_micro()).color(pal::DANGER));
    }
    col.into()
}

/// 当前编辑槽可用的候选（不可候选的槽返回 None）。
fn row_suggestions(app: &App, index: usize) -> Option<(SuggestList, bool)> {
    let query = &app.props.suggest.query;
    match app.props.editing.as_ref()? {
        Slot::Key(i) if *i == index => Some((key_suggestions(app, query), false)),
        Slot::Item(i, _) | Slot::NewItem(i) if *i == index => {
            let key = match app.props.parsed.as_ref() {
                Some((Yaml::Map(pairs), true)) => &pairs.get(*i)?.0,
                _ => return None,
            };
            is_badge_key(key).then(|| (tag_suggestions(app, query), true))
        }
        _ => None,
    }
}

/// 行内候选卡：定高清单（超过可视行数走内滚动），整行可点，键盘高亮只在按过 ↑↓ 后出现。
fn suggest_card<'a>(app: &'a App, list: &SuggestList, tags: bool) -> Element<'a, Message> {
    if list.items.is_empty() {
        // 池非空而过滤无命中：留一行提示，卡片高度不随键入忽隐忽现
        let hint = if tags { "没有匹配的标签" } else { "没有匹配的键名" };
        return suggest_card_frame(
            container(text(hint).size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .height(Length::Fixed(suggest_row_h()))
                .padding(Padding::ZERO.horizontal(6.0))
                .align_y(iced::alignment::Vertical::Center)
                .into(),
        );
    }
    let arms = app.props.suggest.armed;
    // 高亮下标钳到命中范围内：清单在编辑途中变短（词汇表刷新）时 Enter 仍落在被点亮的行上
    let active = app.props.suggest.index.min(list.items.len() - 1);
    let mut rows = Column::<Message>::new().spacing(0);
    for (i, item) in list.items.iter().enumerate() {
        let selected = arms && i == active;
        let fg = if selected { pal::ACCENT } else { pal::TEXT_PRIMARY };
        let label: Element<'a, Message> = if tags {
            row![
                icon("tag", 12.0, if selected { pal::ACCENT } else { pal::TEXT_MUTED }),
                text(format!("#{item}")).size(pal::fs_caption()).color(fg),
            ]
            .spacing(5)
            .align_y(iced::alignment::Vertical::Center)
            .into()
        } else {
            text(item.clone()).size(pal::fs_caption()).color(fg).into()
        };
        rows = rows.push(
            button(
                container(label)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(Padding::ZERO.horizontal(6.0))
                    .align_y(iced::alignment::Vertical::Center),
            )
            .width(Length::Fill)
            .height(Length::Fixed(suggest_row_h()))
            .padding(0.0)
            .style(suggest_row_style(selected))
            .on_press(Message::Props(PropsMsg::SuggestPick(item.clone()))),
        );
    }
    let visible = list.items.len().min(SUGGEST_MAX_ROWS) as f32;
    let list_h = Length::Fixed(visible * suggest_row_h());
    // 溢出的清单挂内滚动区（滚动条只在溢出时占位）；不溢出时直接铺开，省掉无用的滚动状态
    let content: Element<'a, Message> = if list.items.len() > SUGGEST_MAX_ROWS {
        scroll::v(rows)
            .id(app.props.suggest_scroll.clone())
            .width(Length::Fill)
            .height(list_h)
            .into()
    } else {
        rows.height(list_h).into()
    };
    suggest_card_frame(content)
}

/// 候选卡外壳（三级底 + 小圆角，上下留 2px 内衬）。
fn suggest_card_frame<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .width(Length::Fill)
        .padding(Padding::ZERO.vertical(2.0))
        .style(|_t| iced::widget::container::Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_SM),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// 候选行样式：卡片底与 `HOVER` 同色，卡内悬停改叠一档白光（否则悬停无视觉反馈）。
fn suggest_row_style(selected: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_t, status| button::Style {
        background: if selected {
            Some(pal::ACCENT_SOFT.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER_LIFT.into())
        } else {
            None
        },
        text_color: pal::TEXT_PRIMARY,
        border: border::rounded(pal::RADIUS_XS),
        ..button::Style::default()
    }
}

/// 键名候选：常用键剔除已有键名后按子串过滤（不区分大小写）；条数不截断，可视行数由清单钳制。
fn key_suggestions(app: &App, query: &str) -> SuggestList {
    let existing: Vec<String> = match &app.props.parsed {
        Some((Yaml::Map(pairs), true)) => pairs.iter().map(|(k, _)| k.clone()).collect(),
        _ => Vec::new(),
    };
    filter_keys(&existing, query)
}

/// 常用键候选池（剔除已有 → 子串过滤）：keys 池最多 12 条，全命中也不截断。
fn filter_keys(existing: &[String], query: &str) -> SuggestList {
    let q = query.trim().to_ascii_lowercase();
    let mut list = SuggestList::default();
    for key in COMMON_KEYS.iter().filter(|k| !existing.iter().any(|e| e == *k)) {
        list.pool += 1;
        if q.is_empty() || key.to_ascii_lowercase().contains(&q) {
            list.items.push((*key).to_string());
        }
    }
    list
}

/// tags 候选：全仓库词汇表按最后一段过滤、排除已有徽章值与草稿里已键入的项；条数不截断。
fn tag_suggestions(app: &App, query: &str) -> SuggestList {
    let mut existing: Vec<String> = match &app.props.parsed {
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
    existing.extend(split_comma_list(query));
    // 只按最后一段过滤（值输入是逗号分隔的多段）
    let last = query.rsplit(['，', ',']).next().unwrap_or("").trim().to_lowercase();
    filter_tags(app.tag_vocab.iter().map(|row| row.tag.as_str()), &existing, &last)
}

/// 标签候选池（剔除已用 → 最后一段子串过滤）。
fn filter_tags<'a>(
    vocab: impl Iterator<Item = &'a str>,
    used: &[String],
    last: &str,
) -> SuggestList {
    let mut list = SuggestList::default();
    for tag in vocab.filter(|t| !used.iter().any(|e| e == t)) {
        list.pool += 1;
        if last.is_empty() || tag.to_lowercase().contains(last) {
            list.items.push(tag.to_string());
        }
    }
    list
}

/// 候选清单滚动位置（相对比例）：高亮行还在首屏内就不滚，越过可视区后跟着贴底。
fn suggest_scroll_ratio(index: usize, total: usize) -> f32 {
    let visible = total.min(SUGGEST_MAX_ROWS);
    // 不溢出（含空清单）= 无可滚区间
    let scrollable = total - visible;
    if scrollable == 0 {
        return 0.0;
    }
    let first = index.saturating_sub(visible.saturating_sub(1)).min(scrollable);
    first as f32 / scrollable as f32
}

/// 槽位输入框（透明底、聚焦描边；输入统一写 draft，Enter 走 `Submit` 统一决策）。
fn slot_input<'a>(app: &'a App, placeholder: &str, width: Length) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, &app.props.draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(4.0).vertical(2.0))
        .width(width)
        .id(app.props.focus_id.clone())
        .on_input(|s| Message::Props(PropsMsg::Draft(s)))
        .on_submit(Message::Props(PropsMsg::Submit))
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

/// 数组项输入框定宽：按草稿估宽 + 内衬与光标余量（只服务输入宽度，不参与渲染排版口径）。
fn item_input_width(draft: &str) -> Length {
    Length::Fixed((estimate_text_width(draft, pal::fs_caption()) + 20.0).max(44.0))
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

fn open_type_menu(window: iced::window::Id, cursor: Point, key: String) -> Message {
    Message::PopupOpen(
        window,
        crate::layout_view::PopupOpen {
            point: Point::new(cursor.x, cursor.y + 14.0),
            flip_y: Some(cursor.y - 12.0),
            state: PopupState::PropTypeMenu { key },
            min_width: 0.0,
        },
    )
}

// ===== 交互处理 =====

impl App {
    /// 开始编辑一个槽：聚焦当前槽输入框并全选（进入编辑即全选），候选清单回到顶部。
    /// 清单滚动区按位置复用状态，上一槽滚到过的偏移会带进来，故每次开新槽都压回原点。
    fn begin_slot_edit(&self) -> Task<Message> {
        let id = self.props.focus_id.clone();
        operation::focus(id.clone())
            .chain(operation::select_all(id))
            .chain(self.scroll_suggest(0.0))
    }

    fn reset_suggest(&mut self) {
        self.props.suggest = Suggest::default();
    }

    /// 结束槽编辑（不提交）。
    fn cancel_slot_edit(&mut self) {
        self.props.editing = None;
        self.props.draft.clear();
        self.reset_suggest();
    }

    /// 清空槽编辑与面板交互态（切档位 / 撤销重做 / 切文件 / 切仓库）。
    pub(crate) fn clear_props_edit(&mut self) {
        self.props.editing = None;
        self.props.duplicate = None;
        self.props.draft.clear();
        self.props.suggest = Suggest::default();
        self.props.hover_row = None;
        self.props.hover_item = None;
    }

    /// 失焦与动作切换前的统一收尾：有槽编辑则提交（dir 0）。
    /// 返回槽被删空（动作指向的行 / 项身份随之消失，不能再按下标兜底）。
    pub(crate) fn finish_props_edit(&mut self) -> bool {
        if self.props.editing.is_some() {
            self.commit_slot_report(0).1
        } else {
            false
        }
    }

    pub(crate) fn on_props(&mut self, m: PropsMsg) -> Task<Message> {
        match m {
            PropsMsg::PressInside => {
                self.props.press_inside = true;
                Task::none()
            }
            PropsMsg::HoverRow(i) => {
                self.props.hover_row = Some(i);
                Task::none()
            }
            PropsMsg::UnhoverRow(i) => {
                if self.props.hover_row == Some(i) {
                    self.props.hover_row = None;
                }
                Task::none()
            }
            PropsMsg::HoverItem(r, j) => {
                self.props.hover_item = Some((r, j));
                Task::none()
            }
            PropsMsg::UnhoverItem(r, j) => {
                if self.props.hover_item == Some((r, j)) {
                    self.props.hover_item = None;
                }
                Task::none()
            }
            PropsMsg::ToggleCollapse => {
                self.finish_props_edit();
                self.props.collapsed = !self.props.collapsed;
                Task::none()
            }
            PropsMsg::OpenSourceMode => {
                self.clear_props_edit();
                self.handle(Message::NoteMode(NoteMode::Source))
            }
            PropsMsg::StartEdit(slot, row) => {
                let pre = self.props_data().and_then(|d| resolve_row(&d, &row));
                let deleted = self.finish_props_edit();
                let Some(data) = self.props_data() else { return Task::none() };
                // 同一行刚被提交改掉键名（行未删）时键名已变，按渲染下标重定位行级槽
                let resolved = match resolve_slot(&data, &slot, &row) {
                    Some(s) => Some(s),
                    None if !deleted => pre.and_then(|i| slot_at(&data, &slot, i)),
                    None => None,
                };
                let Some(resolved) = resolved else { return Task::none() };
                self.props.editing = Some(resolved);
                self.props.draft = self.slot_text();
                self.props.duplicate = None;
                self.reset_suggest();
                self.begin_slot_edit()
            }
            PropsMsg::Draft(s) => {
                if self.props.editing.is_some() {
                    self.props.draft = s.clone();
                    self.props.suggest.query = s;
                    self.props.suggest.index = 0;
                    self.props.suggest.armed = false;
                    self.props.duplicate = None;
                    // 过滤后清单回到顶部：滚动区留着上次偏移会把命中项藏在视口之外
                    return self.scroll_suggest(0.0);
                }
                Task::none()
            }
            PropsMsg::Submit => {
                if self.props.editing.is_none() {
                    return Task::none();
                }
                if self.props.suggest.armed {
                    return self.pick_suggestion(1);
                }
                self.commit_slot(1)
            }
            PropsMsg::CommitMove(dir) => self.commit_slot(dir),
            PropsMsg::SuggestMove(delta) => {
                let n = self.props_suggestions().len();
                if n > 0 {
                    // 首次按方向键 = 点亮首 / 尾项（armed 前的 index 无意义）
                    self.props.suggest.index = if self.props.suggest.armed {
                        let cur = self.props.suggest.index.min(n - 1) as i32;
                        (cur + delta).clamp(0, n as i32 - 1) as usize
                    } else if delta > 0 {
                        0
                    } else {
                        n - 1
                    };
                    self.props.suggest.armed = true;
                    return self.scroll_suggest(suggest_scroll_ratio(self.props.suggest.index, n));
                }
                Task::none()
            }
            PropsMsg::SuggestPick(text) => self.pick_suggestion_text(text, 0),
            PropsMsg::CancelSlot => {
                self.props.editing = None;
                self.props.draft.clear();
                self.props.duplicate = None;
                self.reset_suggest();
                Task::none()
            }
            PropsMsg::DeleteProp(row) => {
                let pre = self.props_data().and_then(|d| resolve_row(&d, &row));
                let deleted = self.finish_props_edit();
                let i = self.action_row(&row, pre, deleted);
                if let (Some(i), Some(mut data)) = (i, self.props_data()) {
                    data.remove(i);
                    self.apply_props(data);
                }
                self.props.hover_row = None;
                Task::none()
            }
            PropsMsg::DeleteItem(row) => {
                // 同行刚被删空：身份已消失，防按文本重复匹配到另一项上
                let same = self.props_data().is_some_and(|d| match &self.props.editing {
                    Some(Slot::Item(i, _) | Slot::Key(i) | Slot::Value(i)) => {
                        d.get(*i).is_some_and(|(k, _)| k == &row.key)
                    }
                    _ => false,
                });
                let deleted = self.finish_props_edit();
                if !(deleted && same) {
                    if let Some(mut data) = self.props_data() {
                        if let Some(i) = resolve_row(&data, &row) {
                            let Yaml::Seq(items) = &mut data[i].1 else { return Task::none() };
                            let Some((j, _)) = &row.item else { return Task::none() };
                            items.remove(*j);
                            self.apply_props(data);
                        }
                    }
                }
                self.props.hover_item = None;
                Task::none()
            }
            PropsMsg::ToggleBool(row) => {
                let pre = self.props_data().and_then(|d| resolve_row(&d, &row));
                let deleted = self.finish_props_edit();
                let i = self.action_row(&row, pre, deleted);
                if let (Some(i), Some(mut data)) = (i, self.props_data()) {
                    if let Some((_, Yaml::Bool(b))) = data.get_mut(i) {
                        *b = !*b;
                        self.apply_props(data);
                    }
                }
                Task::none()
            }
            PropsMsg::AddItem(row) => {
                let pre = self.props_data().and_then(|d| resolve_row(&d, &row));
                let deleted = self.finish_props_edit();
                let i = self.action_row(&row, pre, deleted);
                let Some(data) = self.props_data() else { return Task::none() };
                let Some(i) = i else { return Task::none() };
                if !matches!(data[i].1, Yaml::Seq(_)) {
                    return Task::none();
                }
                self.props.editing = Some(Slot::NewItem(i));
                self.props.draft.clear();
                self.props.duplicate = None;
                self.reset_suggest();
                self.begin_slot_edit()
            }
            PropsMsg::StartAdd => {
                // 入口可能在弹层里（笔记面板菜单）：先收掉弹层再动手。
                // 弹层留在屏上时，关它的那次按压落在属性面板之外，同批会按「外点失焦」把新建的行收掉
                self.dismiss_popup();
                // 已在编辑槽位：不再插第二条空属性，只把焦点还给当前输入框
                if self.props.editing.is_some() {
                    return operation::focus(self.props.focus_id.clone());
                }
                // 数据取自当前文本而非面板缓存：源码档与缓存滞后的时刻都能拿到真实属性
                let open = frontmatter::block_end(&self.note_text).is_some();
                if !open && self.note_text.starts_with("---") {
                    self.notify(NoticeLevel::Info, "属性块未闭合，请先在源码档修正");
                    return Task::none();
                }
                let parsed = frontmatter::parse(&self.note_text);
                if open && !parsed.ok {
                    self.notify(NoticeLevel::Info, "属性块格式有误，请先在源码档修正");
                    return Task::none();
                }
                let mut data = match parsed.data {
                    Yaml::Map(pairs) if open => pairs,
                    _ => Vec::new(),
                };
                // 占位键立即写盘：面板上立刻多出一条真属性，随后就地改键名与值
                let key = placeholder_key(data.iter().map(|(k, _)| k.as_str()));
                data.push((key.clone(), Yaml::Str(String::new())));
                let index = data.len() - 1;
                self.apply_props(data);
                // 源码档不渲染面板：内容已落盘，用提示替代就地编辑
                if self.note_mode == NoteMode::Source {
                    self.notify(NoticeLevel::Info, format!("已添加空属性「{key}」"));
                    return Task::none();
                }
                self.props.collapsed = false;
                self.props.editing = Some(Slot::Key(index));
                self.props.draft = key;
                self.props.duplicate = None;
                self.reset_suggest();
                self.begin_slot_edit()
            }
            PropsMsg::ConvertType(key, target) => {
                // 来源是属性类型菜单：选完即收，弹层不得留在屏上
                self.dismiss_popup();
                self.finish_props_edit();
                if let Some(mut data) = self.props_data() {
                    if let Some(i) = data.iter().position(|(k, _)| k == &key) {
                        match frontmatter::convert_value(&data[i].1, target) {
                            Some(converted) if converted != data[i].1 => {
                                data[i].1 = converted;
                                self.apply_props(data);
                            }
                            Some(_) => {}
                            None => self.notify(NoticeLevel::Info, "当前值无法转换为该类型"),
                        }
                    }
                }
                Task::none()
            }
            PropsMsg::OpenTypeMenu { key, window, cursor } => {
                let pre = self.props_data().and_then(|d| d.iter().position(|(k, _)| k == &key));
                let deleted = self.finish_props_edit();
                let target = self.props_data().and_then(|d| {
                    d.iter()
                        .position(|(k, _)| k == &key)
                        .or_else(|| if deleted { None } else { pre.filter(|i| *i < d.len()) })
                        .map(|i| d[i].0.clone())
                });
                match target {
                    Some(key) => Task::done(open_type_menu(window, cursor, key)),
                    None => Task::none(),
                }
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

    /// 动作目标行：提交式失焦后先按键名重定位；刚把该行键名改掉（未删行）时按下标兜底。
    fn action_row(&self, row: &RowRef, pre: Option<usize>, deleted: bool) -> Option<usize> {
        let data = self.props_data()?;
        resolve_row(&data, row).or_else(|| {
            if deleted {
                return None;
            }
            pre.filter(|i| *i < data.len())
        })
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

    /// 当前编辑上下文可选的候选（与渲染同一口径）。
    fn props_suggestions(&self) -> Vec<String> {
        let list = match &self.props.editing {
            Some(Slot::Key(i) | Slot::Item(i, _) | Slot::NewItem(i)) => {
                row_suggestions(self, *i).map(|(list, _)| list)
            }
            _ => None,
        };
        list.map(|l| l.items).unwrap_or_default()
    }

    /// 候选清单滚到相对位置（清单未挂滚动区 / 不溢出时无操作）。
    fn scroll_suggest(&self, y: f32) -> Task<Message> {
        operation::snap_to(
            self.props.suggest_scroll.clone(),
            scrollable::RelativeOffset { x: 0.0, y },
        )
    }

    /// Enter：选中当前高亮候选（dir = 提交后前进方向），无候选则走普通提交。
    fn pick_suggestion(&mut self, dir: i32) -> Task<Message> {
        let items = self.props_suggestions();
        let idx = self.props.suggest.index.min(items.len().saturating_sub(1));
        match items.into_iter().nth(idx) {
            Some(text) => self.pick_suggestion_text(text, dir),
            None => self.commit_slot(dir),
        }
    }

    /// 候选选中：编辑槽替换草稿后提交。
    fn pick_suggestion_text(&mut self, text: String, dir: i32) -> Task<Message> {
        self.props.suggest.armed = false;
        if matches!(self.props.editing, Some(Slot::Key(_) | Slot::Item(..) | Slot::NewItem(_))) {
            self.props.draft = text;
        }
        self.commit_slot(dir)
    }

    /// 提交当前编辑槽并按方向移动（槽被删空则不前进；末槽再前进 = 停在原地）。
    fn commit_slot(&mut self, dir: i32) -> Task<Message> {
        self.commit_slot_report(dir).0
    }

    /// `.1` = 槽被删空（行 / 项已消失，调用方的行身份失去兜底依据）。
    fn commit_slot_report(&mut self, dir: i32) -> (Task<Message>, bool) {
        let Some(slot) = self.props.editing.clone() else { return (Task::none(), false) };
        let Some(mut data) = self.props_data() else {
            self.cancel_slot_edit();
            return (Task::none(), false);
        };
        // 槽越界守卫（undo 重建的属性数据可能比编辑发起时少项）：越界 = 取消编辑不提交
        let valid = match &slot {
            Slot::Key(i) | Slot::Value(i) => *i < data.len(),
            Slot::Item(pi, ii) => data.get(*pi).is_some_and(|(_, v)| {
                matches!(v, Yaml::Seq(items) if *ii < items.len())
            }),
            Slot::NewItem(pi) => data.get(*pi).is_some_and(|(_, v)| matches!(v, Yaml::Seq(_))),
        };
        if !valid {
            self.cancel_slot_edit();
            return (Task::none(), false);
        }
        // 键名撞已有键：不写盘、只留提示（同键两行会让后续按键名定位串行）
        if let Slot::Key(i) = slot {
            let name = self.props.draft.trim();
            if !name.is_empty()
                && name != data[i].0
                && data.iter().enumerate().any(|(j, (k, _))| j != i && k == name)
            {
                self.props.duplicate = Some(format!("属性「{name}」已存在，未写入"));
                self.cancel_slot_edit();
                return (Task::none(), false);
            }
        }
        self.props.duplicate = None;
        let draft = std::mem::take(&mut self.props.draft);
        let outcome = commit_slot_data(&mut data, &slot, &draft);
        if outcome.changed {
            self.apply_props(data.clone());
        }
        self.cancel_slot_edit();
        if outcome.deleted || dir == 0 {
            return (Task::none(), outcome.deleted);
        }
        let anchor = match &slot {
            Slot::Key(i) => Some(Slot::Key(*i)),
            Slot::Value(i) => Some(Slot::Value(*i)),
            Slot::Item(pi, ii) => Some(match outcome.item_span {
                Some((_, last)) if dir > 0 => Slot::Item(*pi, last),
                Some((first, _)) => Slot::Item(*pi, first),
                None => Slot::Item(*pi, *ii),
            }),
            Slot::NewItem(_) => None,
        };
        let Some(anchor) = anchor else { return (Task::none(), false) };
        let slots = flatten_slots(&data);
        let target = match slots.iter().position(|s| *s == anchor) {
            Some(p) if dir > 0 => slots.get(p + 1).cloned(),
            Some(p) => p.checked_sub(1).and_then(|q| slots.get(q).cloned()),
            None => None,
        };
        match target {
            Some(next) => {
                self.props.editing = Some(next);
                self.props.draft = self.slot_text();
                (self.begin_slot_edit(), false)
            }
            None => (Task::none(), false),
        }
    }

    /// 用新属性数据重写 frontmatter 并走正文保存链（空映射 = 删块回到无 frontmatter 态）。
    fn apply_props(&mut self, data: Vec<(String, Yaml)>) {
        self.edit.snapshot(&self.note_text);
        let body = crate::strip_frontmatter(&self.note_text).to_string();
        self.note_text = frontmatter::stringify(&Yaml::Map(data), &body);
        // 源码档的编辑面持有自己的文本副本，重写正文后必须同步，否则下一次按键会把属性块写回旧文本
        if self.note_mode == NoteMode::Source {
            self.source_editor = text_editor::Content::with_text(&self.note_text);
        }
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
        self.clear_props_edit();
        self.props.press_inside = false;
        self.refresh_props();
    }
}

/// 按行身份解析行下标；带项身份时校验下标处的文本仍一致（拆项 / 删项后下标会漂移）。
fn resolve_row(data: &[(String, Yaml)], row: &RowRef) -> Option<usize> {
    let i = data.iter().position(|(k, _)| k == &row.key)?;
    if let Some((j, text)) = &row.item {
        let Yaml::Seq(items) = &data[i].1 else { return None };
        if item_text(items.get(*j)?) != *text {
            return None;
        }
    }
    Some(i)
}

/// 按行身份把渲染时的槽重定位到当前数据（动作先提交失焦槽，数据可能已变）。
fn resolve_slot(data: &[(String, Yaml)], slot: &Slot, row: &RowRef) -> Option<Slot> {
    let i = resolve_row(data, row)?;
    match slot {
        Slot::Key(_) => Some(Slot::Key(i)),
        Slot::Value(_) => matches!(data[i].1, Yaml::Str(_)).then_some(Slot::Value(i)),
        Slot::Item(..) => {
            let (j, _) = row.item.as_ref()?;
            let Yaml::Seq(items) = &data[i].1 else { return None };
            scalar_text(items.get(*j)?).map(|_| Slot::Item(i, *j))
        }
        Slot::NewItem(_) => Some(Slot::NewItem(i)),
    }
}

/// 按行下标重定位槽（同行编辑刚改掉键名时使用）；数组项须走文本身份，不做下标兜底。
fn slot_at(data: &[(String, Yaml)], slot: &Slot, i: usize) -> Option<Slot> {
    let (_, value) = data.get(i)?;
    match slot {
        Slot::Key(_) => Some(Slot::Key(i)),
        Slot::Value(_) => matches!(value, Yaml::Str(_)).then_some(Slot::Value(i)),
        Slot::NewItem(_) => matches!(value, Yaml::Seq(_)).then_some(Slot::NewItem(i)),
        Slot::Item(..) => None,
    }
}

/// 槽位提交结果：`item_span` = 数组项提交后占用的下标区间（前进落点用）。
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
}

/// 槽位提交：返回数据是否变化与槽是否被删空。
/// 草稿与原值一致 = 无变化（原本即空也不清删）；键名撞已有键 = 无变化；
/// 单值清空 = 留空属性（可再切类型）；键名 / 数组项 trim 空 = 删；数组项含逗号 = 拆多项替换。
fn commit_slot_data(data: &mut Vec<(String, Yaml)>, slot: &Slot, draft: &str) -> CommitOutcome {
    let value = draft.trim();
    match slot {
        Slot::Key(i) => {
            // 未输入（与原地一致）不写盘：空键可能本来就存在，先判同再判删
            if value == data[*i].0.trim()
                || data.iter().enumerate().any(|(j, (k, _))| j != *i && k == value)
            {
                CommitOutcome::unchanged()
            } else if value.is_empty() {
                data.remove(*i);
                CommitOutcome::removed()
            } else {
                data[*i].0 = value.to_string();
                // 改名落到徽章键：值域是字符串数组，原文字值按逗号拆成项（徽章流只认数组）
                if is_badge_key(value) {
                    if let Yaml::Str(s) = &data[*i].1 {
                        let items: Vec<Yaml> = split_comma_list(s).into_iter().map(Yaml::Str).collect();
                        data[*i].1 = Yaml::Seq(items);
                    }
                }
                CommitOutcome { changed: true, deleted: false, item_span: None }
            }
        }
        Slot::Value(i) => {
            // 草稿未变（含原本就是空串）= 回归原状；改空 = 保留空属性
            let same = matches!(&data[*i].1, Yaml::Str(s) if s.trim() == value);
            if same {
                CommitOutcome::unchanged()
            } else {
                data[*i].1 = Yaml::Str(value.to_string());
                CommitOutcome { changed: true, deleted: false, item_span: None }
            }
        }
        Slot::Item(pi, ii) => {
            let Yaml::Seq(items) = &mut data[*pi].1 else { return CommitOutcome::unchanged() };
            let original = item_text(&items[*ii]);
            if value.is_empty() {
                if original.trim().is_empty() {
                    return CommitOutcome::unchanged();
                }
                items.remove(*ii);
                CommitOutcome::removed()
            } else {
                let parts = split_comma_list(value);
                if parts.len() == 1 {
                    if parts[0] == original.trim() {
                        return CommitOutcome::unchanged();
                    }
                    items[*ii] = Yaml::Str(parts.into_iter().next().unwrap());
                } else {
                    let count = parts.len();
                    items.splice(*ii..=*ii, parts.into_iter().map(Yaml::Str));
                    return CommitOutcome {
                        changed: true,
                        deleted: false,
                        item_span: Some((*ii, *ii + count - 1)),
                    };
                }
                CommitOutcome { changed: true, deleted: false, item_span: Some((*ii, *ii)) }
            }
        }
        Slot::NewItem(pi) => {
            let Yaml::Seq(items) = &mut data[*pi].1 else { return CommitOutcome::unchanged() };
            let parts = split_comma_list(value);
            if parts.is_empty() {
                return CommitOutcome::unchanged();
            }
            let first = items.len();
            items.extend(parts.into_iter().map(Yaml::Str));
            CommitOutcome {
                changed: true,
                deleted: false,
                item_span: Some((first, items.len() - 1)),
            }
        }
    }
}

/// 槽位线性序（键 → 可编辑值 / 数组项）；只读值（布尔 / 嵌套）不入序，前进不落在不可编辑项上。
fn flatten_slots(data: &[(String, Yaml)]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (i, (_, v)) in data.iter().enumerate() {
        slots.push(Slot::Key(i));
        match v {
            Yaml::Str(_) => slots.push(Slot::Value(i)),
            Yaml::Seq(items) => {
                for (j, item) in items.iter().enumerate() {
                    if scalar_text(item).is_some() {
                        slots.push(Slot::Item(i, j));
                    }
                }
            }
            _ => {}
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

/// 中英文逗号拆分（数组项与「改名落到徽章键」的数组展开规则）。
fn split_comma_list(s: &str) -> Vec<String> {
    s.split([',', '，'])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// 新属性的占位键名：`property` 起，重名递增 `property-2`、`property-3`…
/// 占位键先写盘再就地改名，故必须在写盘前就避开已有键（同键两行会让按键名定位串行）。
fn placeholder_key<'a>(existing: impl Iterator<Item = &'a str>) -> String {
    let taken: Vec<&str> = existing.collect();
    if !taken.contains(&"property") {
        return "property".to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("property-{n}");
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<(String, Yaml)> {
        vec![
            ("tags".into(), Yaml::Seq(vec![Yaml::Str("a".into()), Yaml::Str("b".into())])),
            ("status".into(), Yaml::Str("done".into())),
        ]
    }

    fn items(data: &[(String, Yaml)], i: usize) -> Vec<String> {
        match &data[i].1 {
            Yaml::Seq(items) => items.iter().map(item_text).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn unchanged_value_does_not_write() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Value(1), "done");
        assert!(!out.changed && !out.deleted);
        assert_eq!(d[1].1, Yaml::Str("done".into()));
    }

    #[test]
    fn empty_value_keeps_property() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Value(1), "  ");
        assert!(out.changed && !out.deleted);
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].1, Yaml::Str(String::new()));
    }

    #[test]
    fn empty_key_deletes_property() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Key(0), "");
        assert!(out.deleted);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].0, "status");
    }

    #[test]
    fn duplicate_key_rename_is_ignored() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Key(0), "status");
        assert!(!out.changed);
        assert_eq!(d[0].0, "tags");
    }

    #[test]
    fn empty_item_deletes_item() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Item(0, 1), "");
        assert!(out.deleted);
        assert_eq!(items(&d, 0), vec!["a"]);
    }

    #[test]
    fn comma_item_splits_and_reports_span() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Item(0, 1), "b,c，d");
        assert!(out.changed && !out.deleted);
        assert_eq!(out.item_span, Some((1, 3)));
        assert_eq!(items(&d, 0), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn unchanged_item_does_not_write() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::Item(0, 0), " a ");
        assert!(!out.changed);
        assert_eq!(items(&d, 0), vec!["a", "b"]);
    }

    #[test]
    fn unchanged_empty_value_is_kept() {
        let mut d = vec![("note".into(), Yaml::Str(String::new()))];
        let out = commit_slot_data(&mut d, &Slot::Value(0), "");
        assert!(!out.changed && !out.deleted);
        assert_eq!(d, vec![("note".to_string(), Yaml::Str(String::new()))]);
    }

    #[test]
    fn unchanged_empty_item_is_kept() {
        let mut d = vec![
            ("tags".into(), Yaml::Seq(vec![Yaml::Str(String::new()), Yaml::Str("a".into())])),
        ];
        let out = commit_slot_data(&mut d, &Slot::Item(0, 0), "");
        assert!(!out.changed && !out.deleted);
        assert_eq!(items(&d, 0), vec!["", "a"]);
    }

    #[test]
    fn new_item_appends_only_when_non_empty() {
        let mut d = data();
        let out = commit_slot_data(&mut d, &Slot::NewItem(0), "");
        assert!(!out.changed);
        assert_eq!(items(&d, 0), vec!["a", "b"]);

        let out = commit_slot_data(&mut d, &Slot::NewItem(0), "c，d");
        assert!(out.changed);
        assert_eq!(out.item_span, Some((2, 3)));
        assert_eq!(items(&d, 0), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn row_ref_tracks_row_after_removal() {
        let mut d = data();
        d.remove(0);
        assert_eq!(resolve_row(&d, &RowRef::row("status")), Some(0));
        assert!(resolve_row(&d, &RowRef::row("tags")).is_none());
    }

    #[test]
    fn item_row_ref_requires_same_text() {
        let d = data();
        let hit = RowRef { key: "tags".into(), item: Some((0, "a".into())) };
        let miss = RowRef { key: "tags".into(), item: Some((0, "b".into())) };
        let out_of_range = RowRef { key: "tags".into(), item: Some((9, "a".into())) };
        assert_eq!(resolve_row(&d, &hit), Some(0));
        assert!(resolve_row(&d, &miss).is_none());
        assert!(resolve_row(&d, &out_of_range).is_none());
    }

    #[test]
    fn flatten_slots_skips_readonly_values() {
        let d = vec![
            ("count".into(), Yaml::Num(3.0)),
            ("flag".into(), Yaml::Bool(true)),
            ("name".into(), Yaml::Str("x".into())),
        ];
        assert_eq!(flatten_slots(&d), vec![Slot::Key(0), Slot::Key(1), Slot::Key(2), Slot::Value(2)]);
    }

    #[test]
    fn placeholder_key_skips_taken_names() {
        assert_eq!(placeholder_key(std::iter::empty()), "property");
        assert_eq!(placeholder_key(["tags", "status"].into_iter()), "property");
        assert_eq!(placeholder_key(["property"].into_iter()), "property-2");
        assert_eq!(placeholder_key(["property", "property-2"].into_iter()), "property-3");
    }

    #[test]
    fn rename_to_badge_key_makes_array() {
        let mut d = vec![("property".into(), Yaml::Str("a，b".into()))];
        let out = commit_slot_data(&mut d, &Slot::Key(0), "tags");
        assert!(out.changed);
        assert_eq!(d[0].0, "tags");
        assert_eq!(items(&d, 0), vec!["a", "b"]);

        let mut d = vec![("property".into(), Yaml::Str(String::new()))];
        commit_slot_data(&mut d, &Slot::Key(0), "tags");
        assert_eq!(items(&d, 0).len(), 0);
    }

    #[test]
    fn key_pool_excludes_existing_without_truncating() {
        let list = filter_keys(&["tags".to_string()], "");
        assert_eq!(list.pool, COMMON_KEYS.len() - 1);
        assert_eq!(list.items.len(), list.pool);
        assert!(!list.items.iter().any(|k| k == "tags"));
        assert!(!list.hidden());
    }

    #[test]
    fn key_pool_filters_by_substring_ignoring_case() {
        let list = filter_keys(&[], "ST");
        assert_eq!(list.items, vec!["status".to_string()]);
        assert_eq!(list.pool, COMMON_KEYS.len());
    }

    #[test]
    fn empty_pool_hides_card_while_no_hit_keeps_hint() {
        let all: Vec<String> = COMMON_KEYS.iter().map(|k| (*k).to_string()).collect();
        assert!(filter_keys(&all, "").hidden());
        let no_hit = filter_keys(&[], "zzz");
        assert!(no_hit.items.is_empty());
        assert!(!no_hit.hidden());
    }

    #[test]
    fn tag_pool_excludes_used_and_filters_last_segment() {
        let vocab = ["rust", "ruby", "roadmap"];
        let used = vec!["roadmap".to_string()];
        let list = filter_tags(vocab.iter().copied(), &used, "ru");
        assert_eq!(list.items, vec!["rust".to_string(), "ruby".to_string()]);
        assert_eq!(list.pool, 2);
    }

    #[test]
    fn tag_pool_is_not_capped() {
        let vocab: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let list = filter_tags(vocab.iter().map(String::as_str), &[], "");
        assert_eq!(list.items.len(), 20);
        assert_eq!(list.pool, 20);
    }

    #[test]
    fn scroll_ratio_follows_armed_row_past_first_page() {
        assert_eq!(suggest_scroll_ratio(0, 3), 0.0);
        // 首屏 8 行内不外滚
        assert_eq!(suggest_scroll_ratio(3, 20), 0.0);
        // 越过首屏即贴底跟随，末行到底
        assert_eq!(suggest_scroll_ratio(8, 20), 1.0 / 12.0);
        assert_eq!(suggest_scroll_ratio(19, 20), 1.0);
        // 清单不溢出 = 不滚
        assert_eq!(suggest_scroll_ratio(5, 8), 0.0);
    }
}
