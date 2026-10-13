//! ViewNode 声明式控件树：插件提交树描述，宿主解释渲染并回发事件。
//! 控件集为封闭枚举（`kind` 标签），扩容逐版评审；叶子节点不得携带 children。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 一棵界面树。`id` 在同一棵树内必须唯一（宿主 diff 与事件回发都按它定位）；
/// `children` 仅容器类节点可携带（见 [`NodeBody::allows_children`]），`events` 声明该节点要回发哪些事件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewNode {
    pub id: String,
    #[serde(flatten)]
    pub body: NodeBody,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ViewNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventKind>,
}

impl ViewNode {
    /// 宿主挂载前的结构校验：id 非空且全树唯一、叶子无 children。
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        self.validate_into(&mut ids)
    }

    fn validate_into(&self, ids: &mut HashSet<String>) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("节点 id 不能为空".into());
        }
        if !ids.insert(self.id.clone()) {
            return Err(format!("节点 id 重复：{}", self.id));
        }
        if !self.body.allows_children() && !self.children.is_empty() {
            return Err(format!("叶子节点 {}（kind = {}）不得携带 children", self.id, self.body.kind_name()));
        }
        for child in &self.children {
            child.validate_into(ids)?;
        }
        Ok(())
    }
}

/// 节点体：`kind` 标签 + 各自的属性字段。字段全部带默认值，插件只写关心的项。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum NodeBody {
    Row {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
    },
    Column {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
    },
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Axis>,
    },
    Grid {
        columns: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
    },
    /// 可滚动列表：语义等同纵向 scroll + column，为宿主后续做虚拟化预留的分型。
    List {},
    Text {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorToken>,
    },
    TextInput {
        #[serde(default)]
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multiline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<bool>,
    },
    NumberInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Dropdown {
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Checkbox {
        #[serde(default)]
        checked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Button {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ButtonVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
    },
    /// 图标名必须存在于宿主图标形状表（lucide 线性形状），未知名字宿主按可读错误拒绝。
    IconButton {
        icon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
    },
    Image {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
    },
    Progress {
        /// 0.0 ~ 1.0；越界值宿主钳制。
        value: f64,
    },
    Menu {
        items: Vec<MenuItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
}

impl NodeBody {
    /// 容器类节点（可携带 children）：布局容器、滚动容器、网格与列表。
    pub fn allows_children(&self) -> bool {
        matches!(self, Self::Row { .. } | Self::Column { .. } | Self::Scroll { .. } | Self::Grid { .. } | Self::List { .. })
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Row { .. } => "row",
            Self::Column { .. } => "column",
            Self::Scroll { .. } => "scroll",
            Self::Grid { .. } => "grid",
            Self::List { .. } => "list",
            Self::Text { .. } => "text",
            Self::TextInput { .. } => "textInput",
            Self::NumberInput { .. } => "numberInput",
            Self::Dropdown { .. } => "dropdown",
            Self::Checkbox { .. } => "checkbox",
            Self::Button { .. } => "button",
            Self::IconButton { .. } => "iconButton",
            Self::Image { .. } => "image",
            Self::Progress { .. } => "progress",
            Self::Menu { .. } => "menu",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextWeight {
    Normal,
    Medium,
    SemiBold,
}

/// 颜色只允许引用宿主主题 token（插件不得自带色值，主题一致性由此保证）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorToken {
    Text,
    TextSecondary,
    TextMuted,
    Accent,
    Danger,
    Success,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonVariant {
    Primary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    Contain,
    Cover,
    Fill,
}

/// 图片来源：`private` = 插件私有目录内相对路径；`vault` = 仓库根相对路径（宿主按仓库内容读取，
/// 协作空间走同构内容面）。协议不提供任意 URL 直取。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault { path: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuItem {
    pub label: String,
    /// 条目动作标识（须在本菜单内唯一），点击时随 `change` 事件以 [`EventValue::Text`] 回发。
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger: Option<bool>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Self { label: label.into(), action: action.into(), disabled: None, danger: None }
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = Some(true);
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = Some(true);
        self
    }
}

/// 节点事件种类：声明在 [`ViewNode::events`] 里宿主才回发，未声明的事件不上行（省流量、省误触）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Click,
    Change,
    Submit,
    Focus,
    Blur,
}

/// 宿主回发给插件的事件负载。`change`/`submit` 携带新值；菜单条目选择复用 `change`，
/// 值为条目的 `action` 文本（[`EventValue::Text`]）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ViewEvent {
    Click,
    Change { value: EventValue },
    Submit { value: EventValue },
    Focus,
    Blur,
}

/// 事件值的三形态。untagged 按序匹配即可无歧义区分（字符串 / 数值 / 布尔互不重叠）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventValue {
    Text(String),
    Number(f64),
    Bool(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> ViewNode {
        ViewNode {
            id: "root".into(),
            body: NodeBody::Column {
                gap: Some(8.0),
                padding: Some(12.0),
                cross_align: Some(Align::Start),
            },
            children: vec![
                ViewNode {
                    id: "query".into(),
                    body: NodeBody::TextInput {
                        value: "示例文本".into(),
                        placeholder: Some("输入…".into()),
                        multiline: None,
                        password: None,
                    },
                    children: vec![],
                    events: vec![EventKind::Submit],
                },
                ViewNode {
                    id: "go".into(),
                    body: NodeBody::Button {
                        label: "开始".into(),
                        variant: Some(ButtonVariant::Primary),
                        enabled: None,
                    },
                    children: vec![],
                    events: vec![EventKind::Click],
                },
            ],
            events: vec![],
        }
    }

    #[test]
    fn tree_json_uses_kind_tag_and_camel_case() {
        let tree = sample_tree();
        let json = serde_json::to_value(&tree).unwrap();
        assert_eq!(json["kind"], "column");
        assert_eq!(json["crossAlign"], "start");
        assert_eq!(json["children"][0]["kind"], "textInput");
        assert_eq!(json["children"][0]["events"][0], "submit");
        assert_eq!(json["children"][1]["id"], "go");
        // roundtrip 保持一致
        assert_eq!(serde_json::from_value::<ViewNode>(json).unwrap(), tree);
    }

    #[test]
    fn validate_rejects_duplicate_id_and_leaf_children() {
        assert!(sample_tree().validate().is_ok());
        let mut dup = sample_tree();
        dup.children[1].id = "query".into();
        assert!(dup.validate().unwrap_err().contains("重复"));

        let mut leaf = sample_tree();
        leaf.children[1].children = vec![leaf.children[0].clone()];
        assert!(leaf.validate().unwrap_err().contains("不得携带 children"));
    }
}
