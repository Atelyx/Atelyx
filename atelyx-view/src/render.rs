//! 界面树渲染：把 [`ViewNode`] 树解释成 iced 控件，按节点声明绑定事件与交互外观。
//! 宿主实现 [`ViewHost`] 提供节点瞬时状态与外部元素，经 [`ViewMessage`] 把渲染期消息包进自己的消息类型。
//! 交互外观、尺寸上下限与点击命中由 NodeBox 统一收口：布局交给子项，只在子项矩形内绘制并约束尺寸。

use std::borrow::Cow;

use iced::advanced::layout::{Layout, Limits, Node};
use iced::advanced::widget::tree;
use iced::advanced::Renderer as _;
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::mouse;
use iced::touch;
use iced::widget::{
    column, container, grid, image, keyed_column, mouse_area, progress_bar, responsive, row, space,
    text_editor, text_input, Stack,
};
use iced::{Color, Element, Length, Padding, Rectangle, Renderer, Size, Theme, Vector};

use atelyx_plugin_protocol::view::{
    Align, Axis, BoxSize, ButtonVariant, CanvasEventKind, CardTone, ColorSpec, ControlSize,
    EventKind, EventValue, ImageFit, ImageSource, Justify, NodeBody, PaddingSpec, ReportMode,
    RowHeight, SizeSpec, Surface, TextFamily, TextWeight, ViewEvent, ViewNode,
};
use atelyx_ui::controls::{self, Variant};
use atelyx_ui::palette as pal;
use atelyx_ui::scroll;
use atelyx_ui::text;

use crate::support::{declares, node_value_text, report_mode};

/// 容器缺省间距（插件未声明 gap 时）。
const DEFAULT_GAP: f32 = 6.0;
/// 单行输入的内边距（与设置表单同口径）。
const INPUT_PAD_X: f32 = 8.0;
const INPUT_PAD_Y: f32 = 6.0;
/// 多行输入高度。
const EDITOR_H: f32 = 96.0;
/// 画布缺省高度（未给 `height` 也未声明撑满时）。
const CANVAS_DEFAULT_H: f32 = 160.0;
/// 图标缺省边长。
const ICON_DEFAULT_PX: f32 = 16.0;
/// 进度条缺省粗细（与系统标准粗条一致）。
const PROGRESS_DEFAULT_GIRTH: f32 = 30.0;

/// 渲染期消息：宿主实现 [`ViewMessage`] 把它包进自己的消息类型。
#[derive(Debug, Clone)]
pub enum ViewMsg<M> {
    /// 节点事件（点击 / 变更 / 提交 / 滚动 / 画布指针）。
    Event { plugin_id: String, surface_id: String, node_id: String, event: ViewEvent },
    /// 单行 / 数字输入的本地编辑（`commit` = Enter 提交，文本为 None）。
    Draft { plugin_id: String, surface_id: String, node_id: String, text: Option<String>, commit: bool },
    /// 多行输入的编辑器动作（光标 / 选区 / 输入都由宿主编辑器状态承接）。
    TextEdit { plugin_id: String, surface_id: String, node_id: String, action: text_editor::Action },
    /// 节点悬停进出（交互外观与 `hoverOnly` 图标钮据此切换）。
    Hover { plugin_id: String, surface_id: String, node_id: String, entered: bool },
    /// 拖动热区按下：宿主发起整窗拖动。
    WindowDrag(iced::window::Id),
    /// 打开菜单 / 下拉弹层。
    OpenList { window: iced::window::Id, entries: Vec<PopupEntry<M>> },
}

/// 宿主消息契约：渲染期消息经此包进宿主消息类型。
pub trait ViewMessage: Clone + 'static + atelyx_ui::UiHost {
    fn view(msg: ViewMsg<Self>) -> Self;
}

/// 插件菜单 / 下拉的弹层条目：标签 + 选中标记 + 点击要回发的消息。
#[derive(Debug, Clone)]
pub struct PopupEntry<M> {
    pub label: String,
    pub current: bool,
    pub disabled: bool,
    pub danger: bool,
    pub on_press: M,
}

/// 节点身份（插件 id + 面 id + 节点 id）。
pub struct NodeKey<'a> {
    pub plugin_id: &'a str,
    pub surface_id: &'a str,
    pub node_id: &'a str,
}

/// 节点渲染需要的状态种类：只取该节点真正用到的部分，避免每次渲染克隆无关数据。
pub enum StateNeed<'a> {
    /// 仅悬停态。
    Hover,
    /// 单行 / 数字输入：需要合并草稿后的显示值。
    TextInput { plugin_value: &'a str },
    /// 多行输入：需要宿主编辑器状态。
    Multiline,
}

/// 节点瞬时状态（悬停 / 输入显示值 / 多行编辑器）。
pub struct NodeState<'a> {
    pub hovered: bool,
    /// 单行 / 数字输入的显示值（宿主已合并本机草稿）。
    pub text: Option<Cow<'a, str>>,
    pub editor: Option<&'a text_editor::Content>,
}

/// Markdown 节点的渲染请求（解析与排版由宿主按笔记同源实现）。
pub struct MarkdownRequest<'a> {
    pub plugin_id: &'a str,
    pub surface_id: &'a str,
    pub node_id: &'a str,
    /// 字号缩放（缺省 1.0）。
    pub scale: f32,
    /// 已解析的字重 / 字族（None = 正文缺省）。
    pub font: Option<iced::Font>,
    pub color: Option<Color>,
}

/// 画布节点的渲染请求。
pub struct CanvasRequest<'a> {
    pub window: iced::window::Id,
    pub plugin_id: &'a str,
    pub surface_id: &'a str,
    pub node_id: &'a str,
    pub mouse_events: &'a [CanvasEventKind],
}

/// 渲染宿主：节点状态读取与外部元素构建。
pub trait ViewHost<M: Clone + 'static> {
    /// 节点瞬时状态；`need` 之外的字段可为空值。
    fn node_state(&self, key: &NodeKey<'_>, need: StateNeed<'_>) -> NodeState<'_>;
    /// Markdown 正文（宿主经笔记同源解析器渲染；节点未投影时给空元素）。
    fn markdown<'a>(&'a self, request: MarkdownRequest<'_>) -> Element<'a, M>;
    /// 图片句柄（载入由宿主发起；None = 加载中或不可用）。
    fn image(&self, plugin_id: &str, source: &ImageSource) -> Option<image::Handle>;
    /// 自绘画布部件（指令清单由宿主持有）。
    fn canvas<'a>(&'a self, request: CanvasRequest<'_>) -> Element<'a, M>;
}

/// 面身份与所在窗口（弹层与图标钮悬停提示都要窗口 id）；按值持有，元素返回寿命只绑宿主与树。
#[derive(Clone)]
struct Ctx {
    plugin_id: String,
    surface_id: String,
    window: iced::window::Id,
}

/// 渲染一面的根节点：内容按面区域撑满高度（Fill 子项——画布 / 列表——才有可分配空间；
/// Shrink 根列内的 Fill 子项会塌缩成 0 高），`padding` 为宿主外壳给面的内边距。
/// 浮层节点不参与内容排版：宿主按树序把它们铺在整面最上层（含内边距区域）。
pub fn surface<'a, M, H>(
    host: &'a H,
    window: iced::window::Id,
    plugin_id: &str,
    surface_id: &str,
    tree: &'a ViewNode,
    padding: Padding,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let ctx = Ctx { plugin_id: plugin_id.to_string(), surface_id: surface_id.to_string(), window };
    let base: Element<'a, M> = container(render_root(host, &ctx, tree))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(padding)
        .into();
    let mut layers = Vec::new();
    collect_overlays(tree, &mut layers);
    // 始终经由 Stack：浮层开关不改变根控件类型，内容子树的滚动 / 编辑态得以保留。
    let mut stack = Stack::new().push(base);
    for layer in layers {
        stack = stack.push(overlay_layer(host, &ctx, layer));
    }
    stack.width(Length::Fill).height(Length::Fill).into()
}

/// 可滚动节点的稳定 id（插件 id + 面 id + 节点 id；`view.scrollTo` 按它定位）。
pub fn scroll_id(plugin_id: &str, surface_id: &str, node_id: &str) -> String {
    format!("plugin-scroll:{plugin_id}:{surface_id}:{node_id}")
}

fn render_root<'a, M, H>(host: &'a H, ctx: &Ctx, node: &'a ViewNode) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    match &node.body {
        NodeBody::Column { .. } | NodeBody::Row { .. } | NodeBody::Card { .. } | NodeBody::Stack { .. } => {
            render_node_in(host, ctx, node, true)
        }
        _ => render_node_in(host, ctx, node, false),
    }
}

/// 树里的浮层节点（深度优先序，后出现的盖在前面之上）；浮层内容里的浮层同样上提，
/// 因此挂在树里任何一层效果一致。
fn collect_overlays<'a>(node: &'a ViewNode, out: &mut Vec<&'a ViewNode>) {
    for child in &node.children {
        if is_overlay(child) {
            out.push(child);
        }
        collect_overlays(child, out);
    }
}

/// 浮层节点由 [`surface`] 提到面级合成，不进父容器排版。
fn is_overlay(node: &ViewNode) -> bool {
    matches!(node.body, NodeBody::Overlay { .. })
}

/// 单个浮层：铺满整面的遮罩 + 居中内容；声明 `click` 时点击遮罩（内容之外）回发节点点击事件。
fn overlay_layer<'a, M, H>(host: &'a H, ctx: &Ctx, node: &'a ViewNode) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let NodeBody::Overlay { scrim } = &node.body else {
        unreachable!("overlay_layer 只接收浮层节点")
    };
    let scrim = scrim.map_or(pal::SCRIM, spec_color);
    let content: Element<'a, M> = match node.children.first() {
        Some(child) => render_node(host, ctx, child),
        None => space().into(),
    };
    let layer: Element<'a, M> = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .style(move |_| container::Style {
            background: Some(scrim.into()),
            ..container::Style::default()
        })
        .into();
    let on_dismiss =
        declares(node, EventKind::Click).then(|| node_event(ctx, &node.id, ViewEvent::Click));
    ModalLayer::new(layer, on_dismiss).into()
}

/// 单节点解释：容器递归渲染子节点；叶子按控件语义取主题 token 与事件绑定。
fn render_node<'a, M, H>(host: &'a H, ctx: &Ctx, node: &'a ViewNode) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    render_node_in(host, ctx, node, false)
}

fn render_node_in<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    fill_height: bool,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let element = render_body(host, ctx, node, fill_height);
    let element = finish_node(host, ctx, node, element);
    align_node(element, node, fill_height)
}

/// 节点自身外观：交互外观（底色 + 边框）、尺寸上下限、点击命中与悬停上报。
fn finish_node<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    element: Element<'a, M>,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let interaction = node.interaction.as_ref();
    // 按钮与图标钮自带底色与按下态：外观交给控件，`interaction` 在其中只决定可用性
    let owns_press = binds_press_itself(&node.body);
    let wants_hover = !owns_press && interaction.is_some_and(|spec| spec.hover.is_some());
    let skin = if owns_press { None } else { current_surface(host, ctx, node).map(|surface| skin_of(&surface)) };
    let size = node.body.size();
    let min = Size::new(
        size.and_then(|size| size.min_width).unwrap_or(0.0),
        size.and_then(|size| size.min_height).unwrap_or(0.0),
    );
    let max = size.and_then(|size| match (size.max_width, size.max_height) {
        (None, None) => None,
        (width, height) => Some(Size::new(
            width.unwrap_or(f32::INFINITY),
            height.unwrap_or(f32::INFINITY),
        )),
    });
    // 装饰层的存在只随树变化（不随悬停进出）：结构抖动会让子项控件态被重建，滚动位置与焦点随之丢失
    let needs_box = (!owns_press && interaction.is_some())
        || min.width > 0.0
        || min.height > 0.0
        || max.is_some();
    let mut element = element;
    if needs_box {
        element = NodeBox::new(element, skin.unwrap_or_default(), min, max).into();
    }

    let enabled = interaction.is_none_or(|spec| spec.interactive());
    // 按钮与图标钮自身承接按下（含按下态外观），此处不再重复挂命中
    let on_press = (enabled && !owns_press).then(|| click(ctx, node)).flatten();
    let wants_hover = enabled && wants_hover;
    if on_press.is_none() && !wants_hover {
        return element;
    }
    let mut area = mouse_area(element);
    if wants_hover {
        area = area
            .on_enter(hover_msg(ctx, &node.id, true))
            .on_exit(hover_msg(ctx, &node.id, false));
    }
    match on_press {
        Some(message) => area.on_press(message).interaction(mouse::Interaction::Pointer).into(),
        None => area.into(),
    }
}

/// 节点当前状态命中的交互外观（禁用 / 激活 / 悬停 / 常态依次取第一个已声明的档位）。
fn current_surface<M, H>(host: &H, ctx: &Ctx, node: &ViewNode) -> Option<Surface>
where
    H: ViewHost<M>,
    M: Clone + 'static,
{
    let spec = node.interaction.as_ref()?;
    let hovered = spec.hover.is_some() && hovered(host, ctx, &node.id);
    spec.appearance(hovered).copied()
}

/// 节点是否接受交互（`interaction.state` 为 `disabled` 时关掉点击与悬停）。
fn interaction_enabled(node: &ViewNode) -> bool {
    node.interaction.as_ref().is_none_or(|spec| spec.interactive())
}

/// 控件自身承接按下的节点（点击由控件自己的按下处理产生）。
fn binds_press_itself(body: &NodeBody) -> bool {
    matches!(body, NodeBody::Button { .. } | NodeBody::IconButton { .. })
}

/// 列 / 卡片的水平摆放（宽度不足父容器时按 `align` 放到左 / 中 / 右）。
fn align_node<'a, M: 'static>(element: Element<'a, M>, node: &ViewNode, fill_height: bool) -> Element<'a, M> {
    let align = match &node.body {
        NodeBody::Column { align, .. } | NodeBody::Card { align, .. } => *align,
        _ => None,
    };
    align_block(element, align, fill_height)
}

/// 单节点本体的控件语义（不含交互外观与摆放）。
fn render_body<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    fill_height: bool,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    match &node.body {
        NodeBody::Column { gap, padding, cross_align, align: _, size } => {
            let mut col = keyed_column(children(host, ctx, node, Some(Axis::Vertical)))
                .width(column_width(*size))
                .height(column_height(*size, fill_height || grows(node)))
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            if let Some(pad) = padding {
                col = col.padding(padding_of(*pad));
            }
            if let Some(align) = cross_align {
                col = col.align_items(alignment(*align));
            }
            col.into()
        }
        NodeBody::Row { gap, padding, justify, cross_align, size } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let declared = justify.is_some();
            let justify = justify.unwrap_or(Justify::Start);
            let kids = children_elements(host, ctx, node, Some(Axis::Horizontal));
            // spaceBetween：在占满宽度的行里插入弹性空档把子项推向两端
            let mut line = if justify == Justify::SpaceBetween {
                let mut els = Vec::new();
                for (index, child) in kids.into_iter().enumerate() {
                    if index > 0 {
                        els.push(space().width(Length::Fill).into());
                    }
                    els.push(child);
                }
                row(els)
            } else {
                row(kids)
            }
            .spacing(spacing)
            .align_y(cross_align.map(vertical).unwrap_or(Vertical::Center));
            // 占满宽度是「按权重分配子项」与「两端对齐」的前提：行自身收缩就没有可分配的剩余空间
            let fill_width = grows(node) || justify == Justify::SpaceBetween;
            let width = axis_length(
                size.and_then(|size| size.width),
                if fill_width { Length::Fill } else { Length::Shrink },
            );
            line = line
                .width(width)
                .height(axis_length(size.and_then(|size| size.height), if fill_height {
                    Length::Fill
                } else {
                    Length::Shrink
                }));
            if let Some(pad) = padding {
                line = line.padding(padding_of(*pad));
            }
            // 声明了 justify 才谈得上主轴分布（spaceBetween 行自身已撑满，其余套占满宽度的容器）；
            // 未声明时保持 row 原样——宽度收缩，随父列的 cross_align 对齐
            if !declared || justify == Justify::SpaceBetween {
                line.into()
            } else {
                container(line)
                    .width(Length::Fill)
                    .align_x(justify_x(justify))
                    .into()
            }
        }
        NodeBody::Card { gap, padding, accent, tone, radius, bordered, align: _, size } => {
            let inner = keyed_column(children(host, ctx, node, Some(Axis::Vertical)))
                .width(column_width(*size))
                .height(Length::Shrink)
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            // 交互外观接管底色与边框时自身不再绘制（避免两层底色互相盖住）；
            // 当前状态没有对应档位时仍按卡片自己的档位绘制，避免整张卡片消失
            let skin_paints = current_surface(host, ctx, node).is_some();
            let style = if skin_paints {
                container::Style::default()
            } else {
                card_style(accent.unwrap_or_default(), *tone, *radius, *bordered)
            };
            let mut card = container(inner)
                .width(column_width(*size))
                .padding(padding_of(padding.unwrap_or(PaddingSpec::All(10.0))))
                .style(move |_t| style);
            if let Some(height) = card_height(*size, fill_height) {
                card = card.height(height);
            }
            card.into()
        }
        NodeBody::Stack {} => {
            let mut stack = Stack::new();
            for child in children_elements(host, ctx, node, None) {
                stack = stack.push(child);
            }
            let element: Element<'a, M> = stack.into();
            if fill_height {
                container(element).width(Length::Fill).height(Length::Fill).into()
            } else {
                element
            }
        }
        NodeBody::Scroll { direction, gap, height } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let axis = direction.unwrap_or(Axis::Vertical);
            let inner: Element<M> = match axis {
                // 纵向内容撑满视口宽度（自适应换行）；横向内容按自身宽度溢出
                Axis::Horizontal => row(children_elements(host, ctx, node, None)).spacing(spacing).into(),
                Axis::Vertical => column(children_elements(host, ctx, node, None))
                    .width(Length::Fill)
                    .spacing(spacing)
                    .into(),
            };
            let mut scroll = match axis {
                Axis::Horizontal => scroll::h(inner),
                Axis::Vertical => scroll::v(inner),
            }
            .id(node_scroll_id(ctx, &node.id))
            .width(Length::Fill)
            .height(height.map_or(Length::Fill, Length::Fixed));
            if declares(node, EventKind::Scroll) {
                scroll = scroll.on_scroll(scroll_report(ctx, &node.id, axis));
            }
            scroll.into()
        }
        NodeBody::Grid { columns, min_column_width, gap, row_height } => grid_node(
            host,
            ctx,
            node,
            *columns,
            *min_column_width,
            gap.unwrap_or(DEFAULT_GAP),
            *row_height,
        ),
        NodeBody::List { gap } => {
            let mut list = scroll::v(
                keyed_column(children(host, ctx, node, None))
                    .width(Length::Fill)
                    .height(Length::Shrink)
                    .spacing(gap.unwrap_or(DEFAULT_GAP)),
            )
            .id(node_scroll_id(ctx, &node.id))
            .width(Length::Fill)
            .height(Length::Fill);
            if declares(node, EventKind::Scroll) {
                list = list.on_scroll(scroll_report(ctx, &node.id, Axis::Vertical));
            }
            list.into()
        }
        NodeBody::Divider {} => controls::h_divider_subtle(),
        NodeBody::Text { content, size, weight, color, align, family, wrap } => {
            let mut line = text(content.as_str())
                .size(pal::fs_body() * size.unwrap_or(1.0))
                .font(text_font(*weight, *family))
                .color(color.map_or(pal::TEXT_PRIMARY, spec_color));
            if *wrap == Some(false) {
                line = line.wrapping(iced::advanced::text::Wrapping::None);
            }
            // 声明对齐才占满父容器宽度（对齐只在该前提下有意义）；否则按内容宽度收缩，
            // 让 shrink 容器能形成内容自适应气泡
            match align {
                Some(align) => container(line)
                    .width(Length::Fill)
                    .align_x(horizontal(*align))
                    .into(),
                None => line.into(),
            }
        }
        NodeBody::Markdown { size, weight, color, family, .. } => {
            let font = (*weight)
                .map(|weight| text_font(Some(weight), *family))
                .or_else(|| family.map(|family| text_font(None, Some(family))));
            host.markdown(MarkdownRequest {
                plugin_id: &ctx.plugin_id,
                surface_id: &ctx.surface_id,
                node_id: &node.id,
                scale: size.unwrap_or(1.0),
                font,
                color: color.map(spec_color),
            })
        }
        NodeBody::TextInput { placeholder, multiline, password, read_only, .. } => {
            let placeholder = placeholder.as_deref().unwrap_or_default();
            if read_only.unwrap_or(false) {
                let value = node_value_text(&node.body).unwrap_or_default();
                read_only_field(&value, placeholder, password.unwrap_or(false), multiline.unwrap_or(false))
            } else if multiline.unwrap_or(false) {
                multiline_input(host, ctx, node, placeholder)
            } else {
                single_line_input(host, ctx, node, placeholder, password.unwrap_or(false))
            }
        }
        NodeBody::NumberInput { placeholder, read_only, .. } => {
            let placeholder = placeholder.as_deref().unwrap_or_default();
            if read_only.unwrap_or(false) {
                let value = node_value_text(&node.body).unwrap_or_default();
                read_only_field(&value, placeholder, false, false)
            } else {
                single_line_input(host, ctx, node, placeholder, false)
            }
        }
        NodeBody::Dropdown { options, selected, placeholder, read_only } => {
            let label = selected
                .and_then(|index| options.get(index as usize))
                .cloned()
                .or_else(|| placeholder.clone())
                .unwrap_or_else(|| "请选择".to_string());
            let entries = options
                .iter()
                .enumerate()
                .map(|(index, option)| PopupEntry {
                    label: option.clone(),
                    current: *selected == Some(index as u32),
                    disabled: false,
                    danger: false,
                    on_press: node_event(ctx, &node.id, ViewEvent::Change { value: EventValue::Number(index as f64) }),
                })
                .collect();
            let open = (declares(node, EventKind::Change) && !read_only.unwrap_or(false))
                .then(|| M::view(ViewMsg::OpenList { window: ctx.window, entries }));
            controls::dropdown_trigger(&label, Length::Fill, open)
        }
        NodeBody::Checkbox { checked, label, read_only } => {
            let state = if *checked { controls::CheckState::On } else { controls::CheckState::Off };
            let on_press = (declares(node, EventKind::Change) && !read_only.unwrap_or(false)).then(|| {
                node_event(ctx, &node.id, ViewEvent::Change { value: EventValue::Bool(!checked) })
            });
            match label {
                Some(label) => row![
                    controls::checkbox(state, on_press),
                    text(label.as_str()).size(pal::fs_ui()).color(pal::TEXT_PRIMARY),
                ]
                .spacing(6)
                .align_y(Vertical::Center)
                .into(),
                None => controls::checkbox(state, on_press),
            }
        }
        NodeBody::Button { label, icon, variant, enabled, size } => {
            let variant = button_variant(*variant);
            let on_press = (enabled.unwrap_or(true) && interaction_enabled(node))
                .then(|| click(ctx, node))
                .flatten();
            match icon {
                Some(name) => {
                    let glyph = icon_element(ctx, name, 14.0, controls::variant_text_color(variant));
                    controls::btn_with_icon(label.as_str(), glyph, &button_size(*size), variant, on_press).into()
                }
                None => controls::btn(label.as_str(), &button_size(*size), variant, on_press).into(),
            }
        }
        NodeBody::IconButton { icon, tooltip, enabled, size, variant, hover_only } => {
            let (icon_px, box_px) = icon_box(*size);
            let variant = icon_variant(*variant);
            let on_press = (enabled.unwrap_or(true) && interaction_enabled(node))
                .then(|| click(ctx, node))
                .flatten();
            let tip = tooltip.as_ref().map(|label| (ctx.window, label.clone()));
            let icon_el = icon_element(ctx, icon, icon_px, controls::variant_text_color(variant));
            if hover_only.unwrap_or(false) {
                // 悬停才显示：未悬停时占位同尺寸（布局不跳），进出由覆盖热区上报
                let shown = hovered(host, ctx, &node.id);
                let inner: Element<M> = if shown {
                    controls::icon_button_el(icon_el, box_px, variant, None, tip)
                } else {
                    container(space())
                        .width(Length::Fixed(box_px))
                        .height(Length::Fixed(box_px))
                        .into()
                };
                let mut area = mouse_area(inner)
                    .on_enter(hover_msg(ctx, &node.id, true))
                    .on_exit(hover_msg(ctx, &node.id, false));
                // 占位期只留进出热区：不可见的钮不接按住（悬停态先落账，下一帧才带按下目标）
                if shown {
                    if let Some(msg) = on_press {
                        area = area.on_press(msg);
                    }
                }
                area.into()
            } else {
                controls::icon_button_el(icon_el, box_px, variant, on_press, tip)
            }
        }
        NodeBody::Icon { name, size, color } => {
            let color = color.map_or(pal::TEXT_SECONDARY, spec_color);
            icon_element(ctx, name, size.unwrap_or(ICON_DEFAULT_PX), color)
        }
        NodeBody::Image { source, fit, width, height } => image_node(
            host,
            &ctx.plugin_id,
            source,
            fit.unwrap_or(ImageFit::Contain),
            *width,
            *height,
        ),
        NodeBody::Progress { value, girth, tone } => {
            let thickness = girth.unwrap_or(PROGRESS_DEFAULT_GIRTH).max(1.0);
            let bar_color = tone.map_or(pal::ACCENT, spec_color);
            progress_bar(0.0..=1.0, value.clamp(0.0, 1.0) as f32)
                .length(Length::Fill)
                .girth(Length::Fixed(thickness))
                .style(move |_theme| progress_bar::Style {
                    background: pal::BG_SUNKEN.into(),
                    bar: bar_color.into(),
                    border: iced::border::rounded(thickness / 2.0),
                })
                .into()
        }
        NodeBody::Menu { items, label, icon } => {
            let entries = items
                .iter()
                .map(|item| PopupEntry {
                    label: item.label.clone(),
                    current: item.current.unwrap_or(false),
                    disabled: item.disabled.unwrap_or(false),
                    danger: item.danger.unwrap_or(false),
                    on_press: node_event(&ctx, &node.id, ViewEvent::Change { value: EventValue::Text(item.action.clone()) }),
                })
                .collect();
            let open = declares(node, EventKind::Change)
                .then(|| M::view(ViewMsg::OpenList { window: ctx.window, entries }));
            match (label, icon) {
                // 图标 + 文字并存：紧凑的「图标 + 文字」触发钮（非全宽下拉钮）
                (Some(label), Some(icon)) => {
                    let icon_el = icon_element(ctx, icon, 14.0, pal::TEXT_SECONDARY);
                    row![icon_el, controls::dropdown_trigger(label, Length::Shrink, open)]
                        .spacing(4)
                        .align_y(Vertical::Center)
                        .into()
                }
                (Some(label), None) => controls::dropdown_trigger(label, Length::Fill, open),
                (None, Some(icon)) => {
                    let icon_el = icon_element(ctx, icon, 14.0, controls::variant_text_color(Variant::Ghost));
                    controls::icon_button_el(icon_el, controls::ICON_MD, Variant::Ghost, open, None)
                }
                (None, None) => controls::dropdown_trigger("…", Length::Fill, open),
            }
        }
        NodeBody::Canvas { mouse_events, height, fill } => {
            let canvas = host.canvas(CanvasRequest {
                window: ctx.window,
                plugin_id: &ctx.plugin_id,
                surface_id: &ctx.surface_id,
                node_id: &node.id,
                mouse_events,
            });
            let height = if fill.unwrap_or(false) {
                Length::Fill
            } else {
                Length::Fixed(height.unwrap_or(CANVAS_DEFAULT_H))
            };
            container(canvas).width(Length::Fill).height(height).into()
        }
        NodeBody::DragRegion {} => iced::widget::mouse_area(
            container(space())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(M::view(ViewMsg::WindowDrag(ctx.window)))
        .into(),
        // 浮层由 surface 提级合成（见 overlay_layer），不走节点本体渲染
        NodeBody::Overlay { .. } => space().into(),
    }
}

/// 子节点渲染（键 = 节点 id 哈希：插件整树重发时纵向容器按 id 复用控件态，输入焦点与光标不丢）。
/// 横向容器不做按键（iced 无按键行），其控件态依赖结构稳定。
fn children<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    axis: Option<Axis>,
) -> Vec<(u64, Element<'a, M>)>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    node.children
        .iter()
        .filter(|child| !is_overlay(child))
        .map(|child| (node_key(&child.id), child_element(host, ctx, child, axis)))
        .collect()
}

/// 子节点元素序列（行 / 滚动 / 网格容器用）。
fn children_elements<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    axis: Option<Axis>,
) -> Vec<Element<'a, M>>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    node.children
        .iter()
        .filter(|child| !is_overlay(child))
        .map(|child| child_element(host, ctx, child, axis))
        .collect()
}

/// 子节点元素：声明 `grow` 的子项按权重占父容器主轴剩余空间（交叉轴由子项自身尺寸决定）。
fn child_element<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    child: &'a ViewNode,
    axis: Option<Axis>,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let element = render_node(host, ctx, child);
    match (child.grow, axis) {
        (Some(portion), Some(Axis::Horizontal)) => {
            container(element).width(Length::FillPortion(portion)).into()
        }
        (Some(portion), Some(Axis::Vertical)) => {
            container(element).height(Length::FillPortion(portion)).into()
        }
        _ => element,
    }
}

/// 子项是否声明了弹性权重（决定父容器主轴须占满才分配得了剩余空间）；浮层不参与排版。
fn grows(node: &ViewNode) -> bool {
    node.children.iter().any(|child| !is_overlay(child) && child.grow.is_some())
}

/// 列与卡片缺省占满宽度。
fn column_width(size: Option<BoxSize>) -> Length {
    axis_length(size.and_then(|size| size.width), Length::Fill)
}

/// 列与卡片的高度（缺省按内容；根节点与带权重子项的容器撑满）。
fn column_height(size: Option<BoxSize>, fill_height: bool) -> Length {
    axis_length(size.and_then(|size| size.height), if fill_height {
        Length::Fill
    } else {
        Length::Shrink
    })
}

/// 卡片高度：未声明且不需要撑满时不设（保持容器缺省按内容）。
fn card_height(size: Option<BoxSize>, fill_height: bool) -> Option<Length> {
    match size.and_then(|size| size.height) {
        Some(spec) => Some(axis_length(Some(spec), Length::Shrink)),
        None if fill_height => Some(Length::Fill),
        None => None,
    }
}

/// 单轴尺寸声明 → iced 长度（缺省取 `fallback`）。
fn axis_length(spec: Option<SizeSpec>, fallback: Length) -> Length {
    match spec {
        Some(SizeSpec::Fill) => Length::Fill,
        Some(SizeSpec::Shrink) => Length::Shrink,
        Some(SizeSpec::Fixed(value)) => Length::Fixed(value.max(0.0)),
        None => fallback,
    }
}

/// 内边距声明 → iced 内边距（四边取值）。
pub fn padding_of(spec: PaddingSpec) -> Padding {
    let [top, right, bottom, left] = spec.edges();
    Padding { top, right, bottom, left }
}

/// 一层交互外观 → 容器样式（底色 + 1px 描边 + 圆角）。
fn skin_of(surface: &Surface) -> container::Style {
    let radius = surface.radius.unwrap_or(pal::RADIUS_SM).max(0.0);
    let border = match surface.border {
        Some(color) => iced::border::rounded(radius).color(spec_color(color)).width(1.0),
        None => iced::border::rounded(radius),
    };
    container::Style {
        background: surface.background.map(|color| spec_color(color).into()),
        border,
        ..container::Style::default()
    }
}

/// 容器宽度小于父容器时的水平摆放（声明 `align` 才套占满宽度的外层）。
fn align_block<'a, M: 'static>(
    element: Element<'a, M>,
    align: Option<Align>,
    fill_height: bool,
) -> Element<'a, M> {
    match align {
        Some(align) => {
            let mut wrap = container(element)
                .width(Length::Fill)
                .align_x(horizontal(align));
            if fill_height {
                wrap = wrap.height(Length::Fill);
            }
            wrap.into()
        }
        None => element,
    }
}

/// 滚动位置回发（主轴偏移 + 可滚动余量）；相同视口不重复发布由控件自身保证。
fn scroll_report<M: ViewMessage>(
    ctx: &Ctx,
    node_id: &str,
    axis: Axis,
) -> impl Fn(iced::widget::scrollable::Viewport) -> M + 'static {
    let plugin_id = ctx.plugin_id.clone();
    let surface_id = ctx.surface_id.clone();
    let node_id = node_id.to_string();
    move |viewport| {
        let bounds = viewport.bounds();
        let content = viewport.content_bounds();
        let offset = viewport.absolute_offset();
        let (offset, max_offset) = match axis {
            Axis::Horizontal => (offset.x, (content.width - bounds.width).max(0.0)),
            Axis::Vertical => (offset.y, (content.height - bounds.height).max(0.0)),
        };
        M::view(ViewMsg::Event {
            plugin_id: plugin_id.clone(),
            surface_id: surface_id.clone(),
            node_id: node_id.clone(),
            event: ViewEvent::Scroll { offset, max_offset },
        })
    }
}

/// 网格：`columns` 固定列数；`min_column_width` 按可用宽度换列（`columns` 同时给定时作列数上限）。
/// `row_height` 缺省按列宽 1:1 定行高，`content` 让每行按内容高度。
fn grid_node<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    columns: Option<u32>,
    min_column_width: Option<f32>,
    gap: f32,
    row_height: Option<RowHeight>,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    // 浮层不参与网格排版，不计入行数换算与自动列数上限
    let count = node.children.iter().filter(|child| !is_overlay(child)).count().max(1);
    let fixed = columns.map(|columns| columns.max(1) as usize);
    let build = move |host: &'a H, ctx: &Ctx, columns: usize| -> Element<'a, M> {
        let mut grid = grid(children_elements(host, ctx, node, Some(Axis::Horizontal)))
            .columns(columns)
            .spacing(gap);
        match row_height {
            // 内容高度：每行按自身内容（缺省按列宽 1:1，短内容会被拉成方格）
            Some(RowHeight::Content) => grid = grid.height(Length::Shrink),
            // 固定行高：网格把可用高度均分给各行，故按行数换算成整网格高度
            Some(RowHeight::Fixed(value)) => {
                let rows = count.div_ceil(columns.max(1));
                let total = value.max(0.0) * rows as f32 + gap * rows.saturating_sub(1) as f32;
                grid = grid.height(Length::Fixed(total));
            }
            None => {}
        }
        grid.into()
    };
    let Some(min_column_width) = min_column_width else {
        return build(host, ctx, fixed.unwrap_or(1));
    };
    let min_column_width = min_column_width.max(1.0);
    // 列数随可用宽度变化：布局期取实际宽度算列，固定列数只作换列上限。
    // 面身份按值进闭包——元素本身不借它，避免把整棵树的返回寿命绑到局部 `ctx`。
    let ctx = ctx.clone();
    responsive(move |size| {
        let fit = ((size.width + gap) / (min_column_width + gap)).floor().max(1.0) as usize;
        let columns = match fixed {
            Some(fixed) => fit.min(fixed),
            None => fit.min(count),
        };
        build(host, &ctx, columns)
    })
    .height(Length::Shrink)
    .into()
}

/// 表单框外观（多行输入与只读多行共用：同款底色、边框、圆角）。
fn form_box_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
        ..container::Style::default()
    }
}

/// 只读输入：保留表单外观但不接受编辑（单行用禁用态输入框，多行用同款静态框）。
fn read_only_field<'a, M: Clone + 'static>(
    value: &str,
    placeholder: &str,
    password: bool,
    multiline: bool,
) -> Element<'a, M> {
    if !multiline {
        return text_input(placeholder, value)
            .size(pal::fs_ui())
            .font(atelyx_ui::regular())
            .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
            .width(Length::Fill)
            .secure(password)
            .style(|_theme, status| controls::form_input_style(status))
            .into();
    }
    let content = if value.is_empty() {
        text(placeholder.to_string()).size(pal::fs_ui()).color(pal::INPUT_PLACEHOLDER)
    } else {
        text(value.to_string()).size(pal::fs_ui()).color(pal::TEXT_PRIMARY)
    };
    // 固定高度内可滚动：长文不裁切、也不把后面的节点挤出可视区
    container(scroll::v(content).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fixed(EDITOR_H))
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .style(form_box_style)
        .into()
}

/// 节点 id → 容器子项键（`keyed_column` 要求 `Copy + PartialEq`，故用 64 位哈希）。
fn node_key(id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 节点事件消息（面身份随事件一起回发）。
fn node_event<M: ViewMessage>(ctx: &Ctx, node_id: &str, event: ViewEvent) -> M {
    M::view(ViewMsg::Event {
        plugin_id: ctx.plugin_id.clone(),
        surface_id: ctx.surface_id.clone(),
        node_id: node_id.to_string(),
        event,
    })
}

/// 点击事件绑定（节点声明了 click 才挂处理器）。
fn click<M: ViewMessage>(ctx: &Ctx, node: &ViewNode) -> Option<M> {
    declares(node, EventKind::Click).then(|| node_event(ctx, &node.id, ViewEvent::Click))
}

/// 单行 / 数字输入（显示值取宿主草稿合并结果；键入发 change，Enter 发 submit）。
fn single_line_input<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    placeholder: &str,
    password: bool,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let plugin_value = node_value_text(&node.body).unwrap_or_default();
    let value = host
        .node_state(
            &NodeKey { plugin_id: &ctx.plugin_id, surface_id: &ctx.surface_id, node_id: &node.id },
            StateNeed::TextInput { plugin_value: &plugin_value },
        )
        .text
        .unwrap_or(Cow::Borrowed(plugin_value.as_str()));
    let report = report_mode(&node.body);
    let input = text_input(placeholder, value.as_ref())
        .size(pal::fs_ui())
        .font(atelyx_ui::regular())
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .width(Length::Fill)
        .secure(password)
        .style(|_theme, status| controls::form_input_style(status))
        .on_input({
            let ctx = ctx.clone();
            let node_id = node.id.clone();
            move |text| match report {
                ReportMode::Immediate => {
                    node_event(&ctx, &node_id, ViewEvent::Change { value: EventValue::Text(text) })
                }
                _ => M::view(ViewMsg::Draft {
                    plugin_id: ctx.plugin_id.clone(),
                    surface_id: ctx.surface_id.clone(),
                    node_id: node_id.clone(),
                    text: Some(text),
                    commit: false,
                }),
            }
        });
    if report.is_deferred() {
        let plugin_id = ctx.plugin_id.clone();
        let surface_id = ctx.surface_id.clone();
        let node_id = node.id.clone();
        input
            .on_submit(M::view(ViewMsg::Draft {
                plugin_id,
                surface_id,
                node_id,
                text: None,
                commit: true,
            }))
            .into()
    } else if declares(node, EventKind::Submit) {
        input
            .on_submit(node_event(ctx, &node.id, ViewEvent::Submit { value: EventValue::Text(value.into_owned()) }))
            .into()
    } else {
        input.into()
    }
}

/// 多行输入（编辑器状态归宿主：插件不改值则光标与选区原地保留）。
fn multiline_input<'a, M, H>(
    host: &'a H,
    ctx: &Ctx,
    node: &'a ViewNode,
    placeholder: &'a str,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    let Some(state) = host
        .node_state(
            &NodeKey { plugin_id: &ctx.plugin_id, surface_id: &ctx.surface_id, node_id: &node.id },
            StateNeed::Multiline,
        )
        .editor
    else {
        // 编辑器随树投影建立，首帧尚未就绪时按空输入呈现
        return container(text(String::new()))
            .width(Length::Fill)
            .height(Length::Fixed(EDITOR_H))
            .style(form_box_style)
            .into();
    };
    let (plugin_id, surface_id, node_id) = (
        ctx.plugin_id.clone(),
        ctx.surface_id.clone(),
        node.id.clone(),
    );
    text_editor(state)
        .placeholder(placeholder)
        .size(pal::fs_ui())
        .font(atelyx_ui::regular())
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .height(Length::Fixed(EDITOR_H))
        .style(|_theme, _status| text_editor::Style {
            background: pal::BG_TERTIARY.into(),
            border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
            placeholder: pal::INPUT_PLACEHOLDER,
            value: pal::TEXT_PRIMARY,
            selection: pal::SELECTION,
        })
        .on_action(move |action| M::view(ViewMsg::TextEdit {
            plugin_id: plugin_id.clone(),
            surface_id: surface_id.clone(),
            node_id: node_id.clone(),
            action,
        }))
        .into()
}

/// 界面图片：句柄已载入则呈现，否则给可读占位（载入任务由宿主发起）。
/// 尺寸缺省按原图；`fill` 占满父容器该轴。
fn image_node<'a, M, H>(
    host: &H,
    plugin_id: &str,
    source: &ImageSource,
    fit: ImageFit,
    width: Option<SizeSpec>,
    height: Option<SizeSpec>,
) -> Element<'a, M>
where
    M: ViewMessage,
    H: ViewHost<M>,
{
    match host.image(plugin_id, source) {
        Some(handle) => {
            let mut widget = image(handle).content_fit(content_fit(fit));
            if let Some(width) = width.map(axis_length_of) {
                widget = widget.width(width);
            }
            if let Some(height) = height.map(axis_length_of) {
                widget = widget.height(height);
            }
            widget.into()
        }
        None => placeholder("image", "图片", "加载中或不可用"),
    }
}

/// 面内容缺失时的可读占位。
fn placeholder<'a, M: Clone + 'static>(icon_name: &str, title: &str, desc: &str) -> Element<'a, M> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}

fn axis_length_of(spec: SizeSpec) -> Length {
    axis_length(Some(spec), Length::Shrink)
}

/// 自备图标名 → 图标元素：内置形状名直接命中，否则按 `<插件 id>::<名>` 取自备图形。
/// `Ctx` 的插件身份决定命名空间，故同名图形不跨插件串用。
fn icon_element<'a, M: 'static>(ctx: &Ctx, name: &str, px: f32, color: Color) -> Element<'a, M> {
    let key = icon_key(ctx, name);
    atelyx_ui::icon::icon_resolved(&key, px, color, 1.5)
}

/// 图标解析键：内置名原样，未知名按插件命名空间拼限定名（取自备图形，缺省画空）。
fn icon_key(ctx: &Ctx, name: &str) -> String {
    if atelyx_ui::icon::is_builtin(name) {
        return name.to_string();
    }
    format!("{}::{}", ctx.plugin_id, name)
}

/// 文字按钮尺寸档（缺省 `Sm`）。
fn button_size(size: Option<ControlSize>) -> controls::TextSize {
    match size {
        Some(ControlSize::Md) => controls::btn_md(),
        Some(ControlSize::Lg) => controls::btn_lg(),
        _ => controls::btn_sm(),
    }
}

/// 图标钮的（图标边长, 方框边长）档（缺省 `Sm`：14 / 28）。
fn icon_box(size: Option<ControlSize>) -> (f32, f32) {
    match size {
        Some(ControlSize::Md) => (16.0, controls::ICON_LG),
        Some(ControlSize::Lg) => (20.0, controls::ICON_XL),
        _ => (14.0, controls::ICON_MD),
    }
}

/// 卡片样式：底色按档位、边框可关、圆角可调。
fn card_style(
    accent: bool,
    tone: Option<CardTone>,
    radius: Option<f32>,
    bordered: Option<bool>,
) -> container::Style {
    let background = match tone.unwrap_or(CardTone::Default) {
        CardTone::Default => Some(pal::BG_TERTIARY.into()),
        CardTone::Muted => Some(pal::BG_SECONDARY.into()),
        CardTone::AccentSoft => Some(pal::ACCENT_SOFT.into()),
        CardTone::Transparent => None,
    };
    let border = if bordered.unwrap_or(true) {
        iced::border::rounded(radius.unwrap_or(pal::RADIUS_SM))
            .color(if accent { pal::ACCENT } else { pal::BORDER })
            .width(1.0)
    } else {
        iced::border::rounded(radius.unwrap_or(pal::RADIUS_SM)).color(Color::TRANSPARENT).width(0.0)
    };
    container::Style { background, border, ..container::Style::default() }
}

/// 按钮档位映射（缺省 `Secondary`）。
fn button_variant(variant: Option<ButtonVariant>) -> Variant {
    match variant {
        None => Variant::Secondary,
        Some(ButtonVariant::Primary) => Variant::Primary,
        Some(ButtonVariant::Ghost) => Variant::Ghost,
        Some(ButtonVariant::Danger) => Variant::Danger,
    }
}

/// 图标钮档位映射：缺省 `Ghost`（透明底、悬停才出底——工具条图标钮的常态）。
fn icon_variant(variant: Option<ButtonVariant>) -> Variant {
    match variant {
        None => Variant::Ghost,
        Some(ButtonVariant::Primary) => Variant::Primary,
        Some(ButtonVariant::Ghost) => Variant::Ghost,
        Some(ButtonVariant::Danger) => Variant::Danger,
    }
}

fn node_scroll_id(ctx: &Ctx, node_id: &str) -> String {
    scroll_id(&ctx.plugin_id, &ctx.surface_id, node_id)
}

/// 某节点的悬停态（交互外观与 `hoverOnly` 图标钮据此切换）。
fn hovered<M, H>(host: &H, ctx: &Ctx, node_id: &str) -> bool
where
    H: ViewHost<M>,
    M: Clone + 'static,
{
    host.node_state(
        &NodeKey { plugin_id: &ctx.plugin_id, surface_id: &ctx.surface_id, node_id },
        StateNeed::Hover,
    )
    .hovered
}

/// 悬停进出消息。
fn hover_msg<M: ViewMessage>(ctx: &Ctx, node_id: &str, entered: bool) -> M {
    M::view(ViewMsg::Hover {
        plugin_id: ctx.plugin_id.clone(),
        surface_id: ctx.surface_id.clone(),
        node_id: node_id.to_string(),
        entered,
    })
}

fn justify_x(justify: Justify) -> Horizontal {
    match justify {
        Justify::Start | Justify::SpaceBetween => Horizontal::Left,
        Justify::Center => Horizontal::Center,
        Justify::End => Horizontal::Right,
    }
}

/// 插件声明的取色（主题 token 或自定义色）。
pub fn spec_color(spec: ColorSpec) -> Color {
    match spec {
        ColorSpec::Token(name) => token_color(name),
        ColorSpec::Custom(rgba) => Color::from_rgba(rgba.0[0], rgba.0[1], rgba.0[2], rgba.0[3]),
    }
}

/// 主题 token → 主题色（插件控件与画布共用同一映射）。
pub fn token_color(token: atelyx_plugin_protocol::view::ColorToken) -> Color {
    use atelyx_plugin_protocol::view::ColorToken;
    match token {
        ColorToken::Text => pal::TEXT_PRIMARY,
        ColorToken::TextSecondary => pal::TEXT_SECONDARY,
        ColorToken::TextMuted => pal::TEXT_MUTED,
        ColorToken::Accent => pal::ACCENT,
        ColorToken::AccentSoft => pal::ACCENT_SOFT,
        ColorToken::Danger => pal::DANGER,
        ColorToken::DangerSoft => pal::DANGER_SOFT,
        ColorToken::Success => pal::SUCCESS,
        ColorToken::SuccessSoft => pal::SUCCESS_SOFT,
        ColorToken::Warning => pal::WARNING,
        ColorToken::WarningSoft => pal::WARNING_SOFT,
        ColorToken::Surface => pal::BG_TERTIARY,
        ColorToken::SurfaceMuted => pal::BG_SECONDARY,
        ColorToken::SurfaceSunken => pal::BG_SUNKEN,
        ColorToken::Hover => pal::HOVER,
        ColorToken::Border => pal::BORDER,
        ColorToken::BorderStrong => pal::BORDER_STRONG,
        ColorToken::Transparent => Color::TRANSPARENT,
    }
}

/// 字体族与字重 → 字体（等宽族始终取系统等宽字体，不随界面字体族设置变化）。
fn text_font(weight: Option<TextWeight>, family: Option<TextFamily>) -> iced::Font {
    let base = match family {
        Some(TextFamily::Mono) => atelyx_ui::font::mono(),
        _ => atelyx_ui::regular(),
    };
    iced::Font {
        weight: weight.map(weight_of).unwrap_or(iced::font::Weight::Normal),
        ..base
    }
}

fn weight_of(weight: TextWeight) -> iced::font::Weight {
    match weight {
        TextWeight::Normal => iced::font::Weight::Normal,
        TextWeight::Medium => iced::font::Weight::Medium,
        TextWeight::SemiBold => iced::font::Weight::Semibold,
    }
}

fn content_fit(fit: ImageFit) -> iced::ContentFit {
    match fit {
        ImageFit::Contain => iced::ContentFit::Contain,
        ImageFit::Cover => iced::ContentFit::Cover,
        ImageFit::Fill => iced::ContentFit::Fill,
    }
}

fn horizontal(align: Align) -> Horizontal {
    match align {
        Align::Start => Horizontal::Left,
        Align::Center => Horizontal::Center,
        Align::End => Horizontal::Right,
    }
}

fn alignment(align: Align) -> iced::Alignment {
    match align {
        Align::Start => iced::Alignment::Start,
        Align::Center => iced::Alignment::Center,
        Align::End => iced::Alignment::End,
    }
}

fn vertical(align: Align) -> Vertical {
    match align {
        Align::Start => Vertical::Top,
        Align::Center => Vertical::Center,
        Align::End => Vertical::Bottom,
    }
}

/// 节点外观装饰：在子项矩形内绘制底色与边框、按上下限钳制尺寸，布局完全委托子项。
///
/// 不能改用 `container` 包一层：容器自带交叉轴收缩语义，套在 `Fill` 子项或滚动容器上会把布局
/// 压塌。这里的 `layout` 只把子项的尺寸抬到最小、把上限压进 limits，其余照搬子项结果。
struct NodeBox<'a, Message> {
    inner: Element<'a, Message>,
    skin: container::Style,
    min: Size,
    max: Option<Size>,
}

impl<'a, Message> NodeBox<'a, Message> {
    fn new(inner: Element<'a, Message>, skin: container::Style, min: Size, max: Option<Size>) -> Self {
        Self { inner, skin, min, max }
    }
}

impl<'a, Message: Clone + 'static> Widget<Message, Theme, Renderer> for NodeBox<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn state(&self) -> tree::State {
        tree::State::new(())
    }

    fn children(&self) -> Vec<tree::Tree> {
        vec![tree::Tree::new(self.inner.as_widget())]
    }

    fn diff(&self, tree: &mut tree::Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        let limits = match self.max {
            Some(max) => limits.max_width(max.width).max_height(max.height),
            None => *limits,
        };
        let child = self.inner.as_widget_mut().layout(&mut tree.children[0], renderer, &limits);
        let size = child.size();
        let size = Size::new(
            size.width.max(self.min.width),
            size.height.max(self.min.height),
        );
        Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget_mut().operate(&mut tree.children[0], child, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        container::draw_background(renderer, &self.skin, layout.bounds());
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget().draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget().mouse_interaction(&tree.children[0], child, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = layout.children().next()?;
        self.inner
            .as_widget_mut()
            .overlay(&mut tree.children[0], child, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'static> From<NodeBox<'a, Message>> for Element<'a, Message> {
    fn from(node: NodeBox<'a, Message>) -> Self {
        Element::new(node)
    }
}

/// 浮层事件拦截：内容先拿指针事件，其余一律吞掉——下层控件既不响应点击也不滚动。
/// 在内容之外按下并抬起的左键视为遮罩点击，`on_dismiss` 存在时回发。
struct ModalLayer<'a, Message> {
    content: Element<'a, Message>,
    on_dismiss: Option<Message>,
}

/// 遮罩按下记在哪个输入源上：`Some(None)` = 鼠标左键，`Some(Some(finger))` = 该手指。
#[derive(Debug, Default)]
struct ModalState {
    scrim_press: Option<Option<touch::Finger>>,
}

impl<'a, Message> ModalLayer<'a, Message> {
    fn new(content: Element<'a, Message>, on_dismiss: Option<Message>) -> Self {
        Self { content, on_dismiss }
    }
}

impl<'a, Message: Clone + 'static> Widget<Message, Theme, Renderer> for ModalLayer<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ModalState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ModalState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn children(&self) -> Vec<tree::Tree> {
        vec![tree::Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut tree::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(&mut self, tree: &mut tree::Tree, renderer: &Renderer, limits: &Limits) -> Node {
        // 层内容自带 Fill 尺寸（遮罩容器），取给到的完整区域
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = child.size();
        Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        let Some(child) = layout.children().next() else { return };
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], child, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else { return };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() || !blocks_pointer(event) {
            return;
        }
        let state: &mut ModalState = tree.state.downcast_mut();
        let position = pointer_position(event, cursor);
        let over = position.is_some_and(|position| layout.bounds().contains(position));
        let on_content = position.is_some_and(|position| {
            content_bounds(child).is_some_and(|bounds| bounds.contains(position))
        });
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if over {
                    shell.capture_event();
                    if !on_content {
                        state.scrim_press = Some(None);
                    }
                }
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let pressed = state.scrim_press == Some(None);
                if pressed {
                    state.scrim_press = None;
                }
                if over {
                    shell.capture_event();
                }
                if pressed && over && !on_content {
                    publish_dismiss(shell, self.on_dismiss.as_ref());
                }
            }
            iced::Event::Touch(touch::Event::FingerPressed { id, .. }) => {
                if over {
                    shell.capture_event();
                    if !on_content {
                        state.scrim_press = Some(Some(*id));
                    }
                }
            }
            iced::Event::Touch(touch::Event::FingerLifted { id, .. }) => {
                let pressed = state.scrim_press == Some(Some(*id));
                if pressed {
                    state.scrim_press = None;
                }
                if over {
                    shell.capture_event();
                }
                if pressed && over && !on_content {
                    publish_dismiss(shell, self.on_dismiss.as_ref());
                }
            }
            iced::Event::Touch(touch::Event::FingerLost { id, .. }) => {
                if state.scrim_press == Some(Some(*id)) {
                    state.scrim_press = None;
                }
            }
            _ => {
                if over {
                    shell.capture_event();
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let content = layout.children().next().map_or(mouse::Interaction::None, |child| {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                child,
                cursor,
                viewport,
                renderer,
            )
        });
        // 铺满整面且吞掉指针事件：光标在浮层上时报非 None，下层图层不再按悬停着色
        if content == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            content
        }
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let (Some(child), Some(bounds)) =
            (layout.children().next(), layout.bounds().intersection(viewport))
        else {
            return;
        };
        // 内容超出面边界时裁在面内，不画到相邻面板
        renderer.with_layer(bounds, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = layout.children().next()?;
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], child, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'static> From<ModalLayer<'a, Message>> for Element<'a, Message> {
    fn from(layer: ModalLayer<'a, Message>) -> Self {
        Element::new(layer)
    }
}

/// 浮层拦下除光标移动外的指针事件：移动放行到下层，层级游标（levitate）才会让下层悬停退出。
fn blocks_pointer(event: &iced::Event) -> bool {
    match event {
        iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => false,
        iced::Event::Mouse(_) | iced::Event::Touch(_) => true,
        _ => false,
    }
}

/// 浮层内容的实际矩形（内容由外层容器居中摆放，取内层节点边界）。
fn content_bounds(layer: Layout<'_>) -> Option<Rectangle> {
    layer.children().next().map(|content| content.bounds())
}

/// 遮罩点击成立时回发节点点击事件（插件据此收起浮层）。
fn publish_dismiss<Message: Clone>(shell: &mut Shell<'_, Message>, message: Option<&Message>) {
    if let Some(message) = message {
        shell.publish(message.clone());
    }
}

/// 事件发生处的指针坐标：触摸事件自带坐标（触屏没有鼠标游标），其余取游标位置。
fn pointer_position(event: &iced::Event, cursor: mouse::Cursor) -> Option<iced::Point> {
    match event {
        iced::Event::Touch(
            touch::Event::FingerPressed { position, .. }
            | touch::Event::FingerMoved { position, .. }
            | touch::Event::FingerLifted { position, .. }
            | touch::Event::FingerLost { position, .. },
        ) => Some(*position),
        _ => cursor.position(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::view::ColorToken;

    #[test]
    fn node_keys_are_stable_and_distinct() {
        assert_eq!(node_key("status"), node_key("status"), "同一节点 id 的键必须稳定");
        assert_ne!(node_key("status"), node_key("status2"));
    }

    #[test]
    fn axis_length_maps_specs_and_falls_back() {
        assert_eq!(axis_length(None, Length::Shrink), Length::Shrink);
        assert_eq!(axis_length(Some(SizeSpec::Fill), Length::Shrink), Length::Fill);
        assert_eq!(axis_length(Some(SizeSpec::Shrink), Length::Fill), Length::Shrink);
        assert_eq!(axis_length(Some(SizeSpec::Fixed(24.0)), Length::Fill), Length::Fixed(24.0));
        // 负值按 0 处理，不产生反向尺寸
        assert_eq!(axis_length(Some(SizeSpec::Fixed(-4.0)), Length::Fill), Length::Fixed(0.0));
    }

    #[test]
    fn padding_spec_maps_all_four_edges() {
        let axes = padding_of(PaddingSpec::Axes(atelyx_plugin_protocol::view::AxesPadding {
            horizontal: 2.0,
            vertical: 1.0,
        }));
        assert_eq!(axes, Padding { top: 1.0, right: 2.0, bottom: 1.0, left: 2.0 });
        let edges = padding_of(PaddingSpec::Edges(atelyx_plugin_protocol::view::EdgesPadding {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        }));
        assert_eq!(edges, Padding { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 });
    }

    #[test]
    fn skin_carries_background_border_and_radius() {
        let skin = skin_of(&Surface {
            background: Some(ColorSpec::Token(ColorToken::AccentSoft)),
            border: Some(ColorSpec::Token(ColorToken::Accent)),
            radius: Some(8.0),
        });
        assert!(matches!(skin.background, Some(iced::Background::Color(color)) if color == pal::ACCENT_SOFT));
        assert_eq!(skin.border.width, 1.0);
        assert_eq!(skin.border.color, pal::ACCENT);
        assert_eq!(skin.border.radius.top_left, 8.0);
    }

    #[test]
    fn weight_maps_to_real_font_weights() {
        assert_eq!(weight_of(TextWeight::Medium), iced::font::Weight::Medium);
        assert_eq!(weight_of(TextWeight::SemiBold), iced::font::Weight::Semibold);
        // 等宽族：换族但不动字重
        let mono = text_font(Some(TextWeight::Normal), Some(TextFamily::Mono));
        assert!(matches!(mono.family, iced::font::Family::Monospace));
    }

    /// 浮层按深度优先序提到面级：嵌套在任意容器里的浮层都收集得到，先出现的在下。
    #[test]
    fn overlays_are_collected_in_tree_order() {
        let node = |id: &str, body: NodeBody, children: Vec<ViewNode>| ViewNode {
            id: id.into(),
            body,
            interaction: None,
            grow: None,
            children,
            events: vec![],
        };
        let overlay = |id: &str| {
            node(
                id,
                NodeBody::Overlay { scrim: None },
                vec![node("inner", NodeBody::Divider {}, vec![])],
            )
        };
        let tree = node(
            "root",
            NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
            vec![
                node("first", NodeBody::Overlay { scrim: None }, vec![overlay("nested")]),
                node(
                    "box",
                    NodeBody::Card {
                        gap: None,
                        padding: None,
                        accent: None,
                        tone: None,
                        radius: None,
                        bordered: None,
                        align: None,
                        size: None,
                    },
                    vec![overlay("second")],
                ),
            ],
        );
        let mut found = Vec::new();
        collect_overlays(&tree, &mut found);
        assert_eq!(
            found.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(),
            ["first", "nested", "second"]
        );
    }
}
