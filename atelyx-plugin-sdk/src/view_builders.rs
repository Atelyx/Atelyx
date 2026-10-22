//! ViewNode 树构建辅助：薄构造器，字段默认值收拢在此，插件只写关心的项。
//! 事件声明保持显式——构造器不替调用方挂事件，避免「按钮没反应」这类静默偏差。

use crate::protocol::view::{
    BoxSize, ButtonVariant, CanvasEventKind, ColorSpec, EventKind, ImageSource, Interaction,
    MenuItem, NodeBody, ReportMode, SizeSpec, ViewNode,
};

fn node(id: &str, body: NodeBody) -> ViewNode {
    ViewNode {
        id: id.into(),
        body,
        interaction: None,
        grow: None,
        children: Vec::new(),
        events: Vec::new(),
    }
}

/// 给节点补子节点与事件（构造器不自带事件，链式补齐）。
pub trait NodeExt {
    fn with_children(self, children: Vec<ViewNode>) -> ViewNode;
    fn with_events(self, events: Vec<EventKind>) -> ViewNode;
}

impl NodeExt for ViewNode {
    fn with_children(mut self, children: Vec<ViewNode>) -> ViewNode {
        self.children = children;
        self
    }

    fn with_events(mut self, events: Vec<EventKind>) -> ViewNode {
        self.events = events;
        self
    }
}

/// 节点级表述能力：交互外观、弹性权重与容器尺寸，可叠在任意节点上。
pub trait NodeStyleExt {
    fn with_interaction(self, interaction: Interaction) -> ViewNode;
    fn with_grow(self, grow: u16) -> ViewNode;
    /// 设置容器自身尺寸；只有 `column` / `row` / `card` 有此字段，其余节点忽略（尺寸由控件自身语义决定）。
    fn with_size(self, size: BoxSize) -> ViewNode;
}

impl NodeStyleExt for ViewNode {
    fn with_interaction(mut self, interaction: Interaction) -> ViewNode {
        self.interaction = Some(interaction);
        self
    }

    fn with_grow(mut self, grow: u16) -> ViewNode {
        self.grow = Some(grow);
        self
    }

    fn with_size(mut self, size: BoxSize) -> ViewNode {
        match &mut self.body {
            NodeBody::Row { size: slot, .. }
            | NodeBody::Column { size: slot, .. }
            | NodeBody::Card { size: slot, .. } => *slot = Some(size),
            _ => {}
        }
        self
    }
}

/// 主题 token 取色便捷。
pub fn token(color: crate::protocol::view::ColorToken) -> ColorSpec {
    ColorSpec::Token(color)
}

/// 固定像素尺寸便捷。
pub fn px(value: f32) -> SizeSpec {
    SizeSpec::Fixed(value)
}

pub fn row(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(
        id,
        NodeBody::Row { gap: None, padding: None, justify: None, cross_align: None, size: None },
    )
    .with_children(children)
}

pub fn column(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(
        id,
        NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
    )
    .with_children(children)
}

/// 卡片容器：主题底 + 边框 + 圆角的分组盒（底色 / 圆角 / 边框由链式改字段指定）。
pub fn card(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(
        id,
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
    )
    .with_children(children)
}

/// 叠层容器：子节点按提交顺序重叠（后提交的在上），用于「画布当底 + 宿主控件叠其上」。
pub fn stack(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Stack {}).with_children(children)
}

/// 分隔线（主题弱边界色的 1px 水平线）。
pub fn divider(id: &str) -> ViewNode {
    node(id, NodeBody::Divider {})
}

pub fn scroll(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Scroll { direction: None, gap: None, height: None }).with_children(children)
}

/// 固定列数网格。
pub fn grid(id: &str, columns: u32, children: Vec<ViewNode>) -> ViewNode {
    node(
        id,
        NodeBody::Grid {
            columns: Some(columns),
            min_column_width: None,
            gap: None,
            row_height: None,
        },
    )
    .with_children(children)
}

/// 按可用宽度换列的网格：每列不窄于 `min_column_width`，列数随宽度自适应。
pub fn grid_min(id: &str, min_column_width: f32, children: Vec<ViewNode>) -> ViewNode {
    node(
        id,
        NodeBody::Grid {
            columns: None,
            min_column_width: Some(min_column_width),
            gap: None,
            row_height: None,
        },
    )
    .with_children(children)
}

pub fn list(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::List { gap: None }).with_children(children)
}

pub fn text(id: &str, content: impl Into<String>) -> ViewNode {
    node(
        id,
        NodeBody::Text {
            content: content.into(),
            size: None,
            weight: None,
            color: None,
            align: None,
            family: None,
            wrap: None,
        },
    )
}

/// 只读 Markdown 正文（宿主按笔记同源解析器渲染）。
pub fn markdown(id: &str, content: impl Into<String>) -> ViewNode {
    node(
        id,
        NodeBody::Markdown {
            content: content.into(),
            size: None,
            weight: None,
            color: None,
            family: None,
        },
    )
}

pub fn text_input(id: &str, value: impl Into<String>, report: ReportMode) -> ViewNode {
    node(
        id,
        NodeBody::TextInput {
            value: value.into(),
            placeholder: None,
            multiline: None,
            password: None,
            read_only: None,
            report: Some(report),
        },
    )
}

pub fn number_input(id: &str, value: Option<f64>, report: ReportMode) -> ViewNode {
    node(
        id,
        NodeBody::NumberInput {
            value,
            min: None,
            max: None,
            step: None,
            placeholder: None,
            read_only: None,
            report: Some(report),
        },
    )
}

pub fn dropdown(id: &str, options: Vec<String>, selected: Option<u32>) -> ViewNode {
    node(id, NodeBody::Dropdown { options, selected, placeholder: None, read_only: None })
}

pub fn checkbox(id: &str, checked: bool) -> ViewNode {
    node(id, NodeBody::Checkbox { checked, label: None, read_only: None })
}

pub fn button(id: &str, label: impl Into<String>, variant: ButtonVariant) -> ViewNode {
    node(
        id,
        NodeBody::Button {
            label: label.into(),
            icon: None,
            variant: Some(variant),
            enabled: None,
            size: None,
        },
    )
}

pub fn icon_button(id: &str, icon: &str) -> ViewNode {
    node(
        id,
        NodeBody::IconButton {
            icon: icon.into(),
            tooltip: None,
            enabled: None,
            size: None,
            variant: None,
            hover_only: None,
        },
    )
}

pub fn image(id: &str, source: ImageSource) -> ViewNode {
    node(id, NodeBody::Image { source, fit: None, width: None, height: None })
}

pub fn progress(id: &str, value: f64) -> ViewNode {
    node(id, NodeBody::Progress { value, girth: None, tone: None })
}

/// 纯装饰图标（不接受点击；可点的图标用 [`icon_button`]）。
pub fn icon(id: &str, name: &str) -> ViewNode {
    node(id, NodeBody::Icon { name: name.into(), size: None, color: None })
}

pub fn menu(id: &str, label: &str, items: Vec<MenuItem>) -> ViewNode {
    node(id, NodeBody::Menu { items, label: Some(label.into()), icon: None })
}

/// 自绘画布节点：`mouse_events` 声明要接收的指针事件子集（未声明不上行）。
/// 内容经 `Context::submit_canvas` 提交指令清单，命中由插件按指令几何自算。
pub fn canvas(id: &str, mouse_events: Vec<CanvasEventKind>) -> ViewNode {
    node(id, NodeBody::Canvas { mouse_events, height: None, fill: None })
}

/// 拖动热区：自身不渲染内容，用户在其占位区域内按下即由宿主发起整窗拖动
/// （全接管时插件自绘标题栏的拖动把手）。占位大小由父容器布局决定。
pub fn drag_region(id: &str) -> ViewNode {
    node(id, NodeBody::DragRegion {})
}
