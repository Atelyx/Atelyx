//! ViewNode 树构建辅助：薄构造器，字段默认值收拢在此，插件只写关心的项。
//! 事件声明保持显式——构造器不替调用方挂事件，避免「按钮没反应」这类静默偏差。

use crate::protocol::view::{
    ButtonVariant, CanvasEventKind, EventKind, ImageSource, MenuItem, NodeBody, ViewNode,
};

fn node(id: &str, body: NodeBody) -> ViewNode {
    ViewNode { id: id.into(), body, children: Vec::new(), events: Vec::new() }
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

pub fn row(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Row { gap: None, padding: None, align: None, cross_align: None, fill: None })
        .with_children(children)
}

pub fn column(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Column { gap: None, padding: None, cross_align: None }).with_children(children)
}

pub fn scroll(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Scroll { direction: None }).with_children(children)
}

pub fn grid(id: &str, columns: u32, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::Grid { columns, gap: None }).with_children(children)
}

pub fn list(id: &str, children: Vec<ViewNode>) -> ViewNode {
    node(id, NodeBody::List {}).with_children(children)
}

pub fn text(id: &str, content: impl Into<String>) -> ViewNode {
    node(id, NodeBody::Text { content: content.into(), size: None, weight: None, color: None })
}

pub fn text_input(id: &str, value: impl Into<String>) -> ViewNode {
    node(id, NodeBody::TextInput { value: value.into(), placeholder: None, multiline: None, password: None })
}

pub fn number_input(id: &str, value: Option<f64>) -> ViewNode {
    node(id, NodeBody::NumberInput { value, min: None, max: None, step: None, placeholder: None })
}

pub fn dropdown(id: &str, options: Vec<String>, selected: Option<u32>) -> ViewNode {
    node(id, NodeBody::Dropdown { options, selected, placeholder: None })
}

pub fn checkbox(id: &str, checked: bool) -> ViewNode {
    node(id, NodeBody::Checkbox { checked, label: None })
}

pub fn button(id: &str, label: impl Into<String>, variant: ButtonVariant) -> ViewNode {
    node(id, NodeBody::Button { label: label.into(), variant: Some(variant), enabled: None })
}

pub fn icon_button(id: &str, icon: &str) -> ViewNode {
    node(id, NodeBody::IconButton { icon: icon.into(), tooltip: None, enabled: None })
}

pub fn image(id: &str, source: ImageSource) -> ViewNode {
    node(id, NodeBody::Image { source, fit: None })
}

pub fn progress(id: &str, value: f64) -> ViewNode {
    node(id, NodeBody::Progress { value })
}

pub fn menu(id: &str, label: &str, items: Vec<MenuItem>) -> ViewNode {
    node(id, NodeBody::Menu { items, label: Some(label.into()), icon: None })
}

/// 自绘画布节点：`mouse_events` 声明要接收的指针事件子集（未声明不上行）。
/// 内容经 `Context::submit_canvas` 提交指令清单，命中由插件按指令几何自算。
pub fn canvas(id: &str, mouse_events: Vec<CanvasEventKind>) -> ViewNode {
    node(id, NodeBody::Canvas { mouse_events })
}
