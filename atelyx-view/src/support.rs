//! 界面树语义辅助：事件声明与输入值文本，渲染与宿主事件处理共用同一口径。

use atelyx_plugin_protocol::view::{EventKind, NodeBody, ReportMode, ViewNode};

/// 事件是否由节点声明（未声明不上行）。
pub fn declares(node: &ViewNode, kind: EventKind) -> bool {
    node.events.contains(&kind)
}

/// 输入节点的上报档位（缺省逐键）；只读节点不产生事件，档位无意义，按逐键返回。
pub fn report_mode(body: &NodeBody) -> ReportMode {
    match body {
        NodeBody::TextInput { report, .. } | NodeBody::NumberInput { report, .. } => {
            report.unwrap_or(ReportMode::Immediate)
        }
        _ => ReportMode::Immediate,
    }
}

/// 输入节点的值文本（单行取原值、数字按无多余零的十进制；草稿比对与事件回发共用它）。
pub fn node_value_text(body: &NodeBody) -> Option<String> {
    match body {
        NodeBody::TextInput { value, .. } => Some(value.clone()),
        NodeBody::NumberInput { value, .. } => value.map(format_number),
        _ => None,
    }
}

/// 数字的规范文本（整数不带小数点，其余取最短表示）。
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}
