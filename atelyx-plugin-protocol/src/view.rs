//! ViewNode 声明式控件树：插件提交树描述，宿主解释渲染并回发事件。
//! 控件集为封闭枚举（`kind` 标签），扩容逐版评审；叶子节点不得携带 children。
//! 事件值按控件定型：文本 → `text`、数字输入 → `number`（越界按声明钳制）、勾选 → `bool`、
//! 下拉 → `number`（选中项下标）、菜单条目 → `text`（条目的 action）。

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
    /// 树里按 id 找节点（宿主事件定位、画布清单校验、测试共用）。
    pub fn find(&self, node_id: &str) -> Option<&ViewNode> {
        if self.id == node_id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(node_id))
    }

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
        /// 主轴分布：`spaceBetween` 在行占满宽度时把子项推向两端（工具行：左信息 + 右操作）。
        /// 缺省 `start`（子项紧贴左缘）。fill 行内 justify 仍生效。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        justify: Option<Justify>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        /// 撑满语义：`both` = 行占满父容器剩余宽高、Fill 子项均分剩余宽度（如左右分栏）；
        /// `width` = 仅占满宽度（高度仍按内容，用于工具行左右分布）。
        /// 缺省行宽收缩、子项 Fill 按各自内容宽解析。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Fill>,
    },
    Column {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<NodeWidth>,
        /// 最大宽度（px）：配合 `align` 得到居中的窄阅读列，配合 `shrink` 形成内容自适应气泡。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
    },
    /// 卡片容器：主题化背景 + 边框 + 圆角的分组盒（列表条目、信息分组）。
    Card {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<f32>,
        /// 强调态（列表选中项）：accent 边框。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<bool>,
        /// 底色档位（缺省主题卡底）。插件只选主题档位，不写死色值。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<CardTone>,
        /// 圆角半径（px）；缺省用宿主卡片圆角。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<f32>,
        /// 是否画边框；缺省 true。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bordered: Option<bool>,
        /// 缺省 `fill`（同 `Column`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<NodeWidth>,
        /// 最大宽度（px）：配合 `align` 得到居中的窄卡片。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
    },
    /// 叠层：子节点按提交顺序重叠（后提交的在上），同尺寸对齐于左上角。
    /// 用于「自绘画布当底 + 宿主控件叠其上」或徽标叠图。
    Stack {},
    Scroll {
        /// 子节点按主轴单向排列：`horizontal` 排成行，其余（含缺省）排成列。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Axis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 交叉轴高度（px）：纵向滚动时收住自身高度，横向滚动时按该高度做图条；
        /// 缺省占满父容器剩余高度。节点声明 `scroll` 事件即上报滚动位置。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
    },
    /// 网格：`columns` 固定列数；`min_column_width` 按可用宽度换列（`columns` 同时给定时作列数上限）。
    /// 二者都缺省按单列渲染。
    Grid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        columns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_column_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
    },
    /// 可滚动列表：语义等同纵向 scroll + column（宿主对子项按键复用控件态）。
    /// 位置可用 `view.scrollTo` 指令控制；声明 `scroll` 事件即上报滚动位置。
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
    },
    Text {
        content: String,
        /// 字号倍率（相对界面基准正文字号；缺省 1.0）。跟界面字号缩放，不是绝对像素。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorToken>,
        /// 水平对齐：声明后节点占满父容器并按该值摆放入文本；缺省按内容宽度收缩。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
    },
    /// 只读 Markdown 正文：宿主按笔记同源解析器渲染（标题 / 列表 / 引用 / 代码块 / 表格 / 链接）。
    /// 块级图片与笔记同源载入：http(s) 走下载，其余按仓库相对路径。
    /// 字重与取色默认走宿主主题与 Markdown 语义（标题层级 / 强调），`weight` / `color` 作整体覆盖。
    /// 链接可点击：宿主按形态默认打开（外链 / wiki / 仓库路径），节点声明 `linkClick` 时同时回发事件。
    /// 无编辑态；其余交互请用其他控件。
    Markdown {
        content: String,
        /// 整体字号倍率（相对界面基准正文字号；缺省 1.0）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        /// 基础字重（标题与加粗片段恒为加粗，不受此项影响）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        /// 基础文字色（链接 / 代码块等语义色不受影响）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorToken>,
    },
    TextInput {
        #[serde(default)]
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 多行：宿主按多行编辑器渲染，Enter 换行（`submit` 事件只对单行生效）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multiline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<bool>,
        /// 只读：保留输入控件外观但不接受编辑，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
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
        /// 只读：保留输入控件外观但不接受编辑、无步进，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
    },
    Dropdown {
        options: Vec<String>,
        /// 选中项下标；`change` 事件的值同为下标（[`EventValue::Number`]）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 只读：保留触发钮外观但不展开、不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
    },
    Checkbox {
        #[serde(default)]
        checked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// 只读：保留勾选态展示但不响应点击。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
    },
    Button {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ButtonVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// 尺寸档位（缺省 Sm，高 24 的紧凑钮）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ControlSize>,
    },
    /// 图标名：宿主内置形状（lucide 线性形状）或经 `view.defineIcon` 定义的自备名（内置名优先）；
    /// 两者都未命中时画空。
    IconButton {
        icon: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// 尺寸档位（缺省 Sm：14px 图标 + 28 方框）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ControlSize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ButtonVariant>,
        /// 悬停才显示（列表行的操作钮常驻会喧宾夺主）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hover_only: Option<bool>,
    },
    /// 图片：`width` / `height` 给定时按该尺寸渲染（数字 = px，`"fill"` = 撑满父容器该轴；
    /// 缺省按原图尺寸）；`fit` 决定尺寸内的适配方式，缺省 `contain`（不裁切不拉伸）。
    /// 节点声明 `click` 事件即成为可点击图片（大图预览入口）。
    Image {
        source: ImageSource,
        /// 尺寸内适配：`contain` 完整可见、`cover` 铺满并裁掉溢出、`fill` 拉伸铺满。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<Dimension>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<Dimension>,
    },
    Progress {
        /// 0.0 ~ 1.0；越界值宿主钳制。
        value: f64,
    },
    /// 菜单：`label` 与 `icon` 可并存（并存时渲染为紧凑的「图标 + 文字」触发钮，非全宽下拉钮）；
    /// 只给 `icon` 为纯图标触发钮，只给 `label` 为全宽下拉钮。二者都缺省时回落到 `…` 触发钮。
    Menu {
        items: Vec<MenuItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// 自绘画布：宿主保留一块区域执行 `canvas.submit` 提交的指令清单（见 [`crate::canvas`]）。
    /// 指针事件按 `mouseEvents` 声明子集回发，未声明不上行；命中由插件按指令几何自算。
    /// `height` 给定时取该高度；`fill` = 占满父容器剩余空间（放在 Fill 容器里才有可分配空间）。
    /// 画布尺寸变化时宿主回发 `host:canvas.resized`（插件据此对齐绘制坐标与滚动范围）。
    Canvas {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mouse_events: Vec<CanvasEventKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<bool>,
    },
    /// 分隔线：主题弱边界色的 1px 水平线（分组容器内的轻分割）。
    Divider {},
    /// 拖动热区：自身不渲染任何内容，用户在其占位区域内按下即由宿主发起整窗拖动
    /// （全接管时插件自绘标题栏的拖动把手）。事件不回发。
    DragRegion {},
}

impl NodeBody {
    /// 容器类节点（可携带 children）：布局容器、滚动容器、网格与列表。
    pub fn allows_children(&self) -> bool {
        matches!(
            self,
            Self::Row { .. }
                | Self::Column { .. }
                | Self::Card { .. }
                | Self::Stack { .. }
                | Self::Scroll { .. }
                | Self::Grid { .. }
                | Self::List { .. }
        )
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Row { .. } => "row",
            Self::Column { .. } => "column",
            Self::Card { .. } => "card",
            Self::Stack { .. } => "stack",
            Self::Scroll { .. } => "scroll",
            Self::Grid { .. } => "grid",
            Self::List { .. } => "list",
            Self::Text { .. } => "text",
            Self::Markdown { .. } => "markdown",
            Self::TextInput { .. } => "textInput",
            Self::NumberInput { .. } => "numberInput",
            Self::Dropdown { .. } => "dropdown",
            Self::Checkbox { .. } => "checkbox",
            Self::Button { .. } => "button",
            Self::IconButton { .. } => "iconButton",
            Self::Image { .. } => "image",
            Self::Progress { .. } => "progress",
            Self::Menu { .. } => "menu",
            Self::Canvas { .. } => "canvas",
            Self::Divider { .. } => "divider",
            Self::DragRegion { .. } => "dragRegion",
        }
    }
}

/// 行主轴分布（[`NodeBody::Row`] 的 `justify`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Justify {
    Start,
    Center,
    End,
    /// 两端对齐：行占满宽度时首个贴左、末个贴右，中间拉开（工具行）。
    SpaceBetween,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Align {
    Start,
    Center,
    End,
}

/// 行的撑满语义（[`NodeBody::Row`] 的 `fill`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fill {
    /// 宽高都撑满（分栏行：Fill 子项均分剩余宽度、行占满剩余高度）。
    Both,
    /// 仅宽度撑满（高度仍按内容：工具行的左右分布）。
    Width,
}

/// 容器宽度语义（[`NodeBody::Column`] / [`NodeBody::Card`] 的 `width`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeWidth {
    /// 占满父容器可用宽度（缺省）。
    Fill,
    /// 按内容宽度收缩（右对齐气泡、贴内容边界的卡片）。
    Shrink,
}

/// 内边距声明：数字 = 四边等距；`{horizontal, vertical}` = 分轴；`{top, right, bottom, left}` = 分边。
/// 两个对象形态都不接受未声明字段，避免分边写法被分轴形态吞掉。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaddingSpec {
    All(f32),
    Axes(AxesPadding),
    Edges(EdgesPadding),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AxesPadding {
    #[serde(default)]
    pub horizontal: f32,
    #[serde(default)]
    pub vertical: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdgesPadding {
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub bottom: f32,
    #[serde(default)]
    pub left: f32,
}

impl PaddingSpec {
    /// 四边取值（上 / 右 / 下 / 左）；宿主据此校验并换算成自身布局类型。
    pub fn edges(self) -> [f32; 4] {
        match self {
            Self::All(value) => [value; 4],
            Self::Axes(axes) => [axes.vertical, axes.horizontal, axes.vertical, axes.horizontal],
            Self::Edges(edges) => [edges.top, edges.right, edges.bottom, edges.left],
        }
    }
}

/// 图片单轴尺寸：数字 = 固定像素；`"fill"` = 占满父容器该轴。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dimension {
    Fixed(f32),
    Fill,
}

impl Serialize for Dimension {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(value) => serializer.serialize_f32(*value),
            Self::Fill => serializer.serialize_str("fill"),
        }
    }
}

impl<'de> Deserialize<'de> for Dimension {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DimensionVisitor;

        impl<'de> serde::de::Visitor<'de> for DimensionVisitor {
            type Value = Dimension;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("像素数字或 \"fill\"")
            }

            fn visit_f32<E: serde::de::Error>(self, value: f32) -> Result<Dimension, E> {
                Ok(Dimension::Fixed(value))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Dimension, E> {
                Ok(Dimension::Fixed(value as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Dimension, E> {
                Ok(Dimension::Fixed(value as f32))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Dimension, E> {
                Ok(Dimension::Fixed(value as f32))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Dimension, E> {
                match value {
                    "fill" => Ok(Dimension::Fill),
                    other => Err(E::unknown_variant(other, &["fill"])),
                }
            }
        }

        deserializer.deserialize_any(DimensionVisitor)
    }
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

/// 卡片底色档位（[`NodeBody::Card`] 的 `tone`）。插件只选主题档位，不写死色值。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardTone {
    /// 默认卡片底（比面板底色略深一档）。
    Default,
    /// 更淡的底（次级信息分组）。
    Muted,
    /// 强调软底（accent 的低饱和底）。
    AccentSoft,
    /// 无底色（只用边框分组）。
    Transparent,
}

/// 控件尺寸档位。`Sm` = 紧凑（Button 高 24、IconButton 14px 图标 + 28 方框）；
/// `Md` = 常规（Button 高 32、IconButton 16px 图标 + 32 方框）；`Lg` = 宽松。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlSize {
    Sm,
    Md,
    Lg,
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
    /// 当前项标记（宿主在条目前画勾）：用于「历史会话列表」这类单选菜单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Self { label: label.into(), action: action.into(), disabled: None, danger: None, current: None }
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = Some(true);
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = Some(true);
        self
    }

    /// 标记为当前项（宿主在条目前画勾）。
    pub fn current(mut self) -> Self {
        self.current = Some(true);
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
    /// Markdown 正文内的链接点击（仅 `markdown` 节点；宿主默认打开行为不受声明影响）。
    LinkClick,
    /// 滚动位置变化（仅 `scroll` / `list` 节点）：滚动时回发当前偏移与可滚动余量；
    /// 内容不足一屏时没有滚动，不发事件。
    Scroll,
}

/// 宿主回发给插件的事件负载。`change`/`submit` 携带新值；菜单条目选择复用 `change`，
/// 值为条目的 `action` 文本（[`EventValue::Text`]）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum ViewEvent {
    Click,
    Change { value: EventValue },
    Submit { value: EventValue },
    Focus,
    Blur,
    /// Markdown 链接点击（仅声明 `linkClick` 的节点）。`href` 为源文本原样（未解码；
    /// wiki 为笔记名），`form` 与宿主笔记链接解析同一口径。
    LinkClick { href: String, form: LinkForm },
    /// 画布指针事件（仅 `canvas` 节点按 `mouseEvents` 声明后回发）。
    Canvas { pointer: CanvasPointer },
    /// 滚动位置（仅声明 `scroll` 的 `scroll` / `list` 节点）：`offset` = 主轴当前偏移，
    /// `max_offset` = 主轴可滚动余量；`offset >= max_offset` 即停在末端。
    Scroll { offset: f32, max_offset: f32 },
}

/// Markdown 链接形态：外链 / wiki / 仓库路径 / 可新建 / 未识别（保持原文）。
/// `create` / `plain` 在 Markdown 正文里不可点击，`linkClick` 只会带来前三种。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkForm {
    External,
    Wiki,
    Path,
    Create,
    Plain,
}

/// 事件值的四形态。untagged 按序匹配即可无歧义区分（字符串 / 数值 / 布尔 / null 互不重叠）；
/// 数字输入清空时上报 `Null`，与「值为 0」区分。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventValue {
    Text(String),
    Number(f64),
    Bool(bool),
    Null,
}

/// 画布指针事件种类：节点 `mouseEvents` 声明要收的子集，宿主只回发声明的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CanvasEventKind {
    PointerDown,
    PointerMove,
    PointerUp,
    PointerLeave,
    Wheel,
    DoubleClick,
}

impl CanvasEventKind {
    /// 指针负载是否属于该事件种类（宿主回发前按节点声明过滤）。
    pub fn matches(self, pointer: &CanvasPointer) -> bool {
        matches!(
            (self, pointer),
            (Self::PointerDown, CanvasPointer::Down { .. })
                | (Self::PointerMove, CanvasPointer::Move { .. })
                | (Self::PointerUp, CanvasPointer::Up { .. })
                | (Self::PointerLeave, CanvasPointer::Leave)
                | (Self::Wheel, CanvasPointer::Wheel { .. })
                | (Self::DoubleClick, CanvasPointer::DoubleClick { .. })
        )
    }
}

/// 鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 画布指针事件负载。坐标 = 节点本地逻辑像素、左上原点；
/// wheel 增量为行数，正 = 向下 / 向右（Web 惯例，宿主负责与平台滚动方向换算）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum CanvasPointer {
    Down { x: f32, y: f32, button: MouseButton },
    Move { x: f32, y: f32 },
    Up { x: f32, y: f32, button: MouseButton },
    Leave,
    Wheel { x: f32, y: f32, delta_x: f32, delta_y: f32 },
    /// 宿主判定（同键二次按下、间隔与位移都在阈值内），插件无需自己算。
    DoubleClick { x: f32, y: f32, button: MouseButton },
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
                width: None,
                max_width: None,
                align: None,
            },
            children: vec![
                ViewNode {
                    id: "query".into(),
                    body: NodeBody::TextInput {
                        value: "示例文本".into(),
                        placeholder: Some("输入…".into()),
                        multiline: None,
                        password: None,
                        read_only: None,
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
                        size: None,
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
    fn event_value_null_roundtrips_apart_from_zero() {
        assert_eq!(serde_json::to_value(EventValue::Null).unwrap(), serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<EventValue>(serde_json::Value::Null).unwrap(),
            EventValue::Null
        );
        assert_eq!(serde_json::to_value(EventValue::Number(0.0)).unwrap(), serde_json::json!(0.0));
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

    #[test]
    fn row_fill_field_roundtrips_and_defaults_off() {
        let fill = NodeBody::Row {
            gap: None,
            padding: None,
            justify: None,
            cross_align: None,
            fill: Some(Fill::Both),
        };
        let json = serde_json::to_value(&fill).unwrap();
        assert_eq!(json["fill"], "both");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), fill);

        let width_only = NodeBody::Row {
            gap: None,
            padding: None,
            justify: None,
            cross_align: None,
            fill: Some(Fill::Width),
        };
        assert_eq!(
            serde_json::to_value(&width_only).unwrap()["fill"],
            "width"
        );

        // 旧树没有 fill 字段：反序列化照常，行为按缺省（收缩）处理
        let legacy: NodeBody =
            serde_json::from_value(serde_json::json!({ "kind": "row" })).unwrap();
        assert_eq!(
            legacy,
            NodeBody::Row { gap: None, padding: None, justify: None, cross_align: None, fill: None }
        );
    }

    /// 行主轴两端对齐与文本水平对齐走独立枚举（不与交叉轴 Align 混用）。
    #[test]
    fn justify_and_card_tone_roundtrip() {
        let row = NodeBody::Row {
            gap: None,
            padding: None,
            justify: Some(Justify::SpaceBetween),
            cross_align: None,
            fill: Some(Fill::Width),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["justify"], "spaceBetween");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), row);

        let text = NodeBody::Text {
            content: "右对齐".into(),
            size: Some(1.25),
            weight: None,
            color: None,
            align: Some(Align::End),
        };
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(json["align"], "end");
        assert_eq!(json["size"], 1.25);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), text);

        let card = NodeBody::Card {
            gap: Some(4.0),
            padding: None,
            accent: None,
            tone: Some(CardTone::AccentSoft),
            radius: Some(12.0),
            bordered: Some(false),
            width: None,
            max_width: None,
            align: None,
        };
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["tone"], "accentSoft");
        assert_eq!(json["bordered"], false);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), card);
    }

    /// 新增分型：Stack（叠层容器）可携带 children，Divider 为叶子。
    #[test]
    fn stack_allows_children_and_divider_is_leaf() {
        let stack = NodeBody::Stack {};
        assert!(stack.allows_children());
        assert_eq!(stack.kind_name(), "stack");
        let divider = NodeBody::Divider {};
        assert!(!divider.allows_children());
        assert_eq!(divider.kind_name(), "divider");
        let with_children = ViewNode {
            id: "s".into(),
            body: stack,
            children: vec![ViewNode {
                id: "d".into(),
                body: divider,
                children: vec![],
                events: vec![],
            }],
            events: vec![],
        };
        assert!(with_children.validate().is_ok());
    }

    #[test]
    fn canvas_event_kind_matches_only_its_payload() {
        use CanvasEventKind::*;
        let down = CanvasPointer::Down { x: 1.0, y: 2.0, button: MouseButton::Left };
        assert!(PointerDown.matches(&down));
        assert!(!PointerUp.matches(&down));
        assert!(!Wheel.matches(&down));
        assert!(Wheel.matches(&CanvasPointer::Wheel { x: 0.0, y: 0.0, delta_x: 0.0, delta_y: 1.0 }));
        assert!(DoubleClick.matches(&CanvasPointer::DoubleClick { x: 0.0, y: 0.0, button: MouseButton::Right }));
    }

    /// 分轴与分边共用对象形态：分边写法必须落到 `Edges`，不能被 `Axes` 忽略多余字段吞掉。
    #[test]
    fn padding_spec_distinguishes_axes_from_edges() {
        let all: PaddingSpec = serde_json::from_value(serde_json::json!(8.0)).unwrap();
        assert_eq!(all, PaddingSpec::All(8.0));
        let axes: PaddingSpec =
            serde_json::from_value(serde_json::json!({ "horizontal": 6.0, "vertical": 3.0 })).unwrap();
        assert_eq!(axes, PaddingSpec::Axes(AxesPadding { horizontal: 6.0, vertical: 3.0 }));
        let edges: PaddingSpec =
            serde_json::from_value(serde_json::json!({ "top": 4.0, "left": 2.0 })).unwrap();
        assert_eq!(
            edges,
            PaddingSpec::Edges(EdgesPadding { top: 4.0, right: 0.0, bottom: 0.0, left: 2.0 })
        );
        assert!(serde_json::from_value::<PaddingSpec>(
            serde_json::json!({ "top": 4.0, "horizontal": 2.0 })
        )
        .is_err());
    }

    #[test]
    fn dimension_accepts_pixels_and_fill_keyword() {
        let image = NodeBody::Image {
            source: ImageSource::Vault { path: "a.png".into() },
            fit: Some(ImageFit::Contain),
            width: Some(Dimension::Fill),
            height: Some(Dimension::Fixed(120.0)),
        };
        let json = serde_json::to_value(&image).unwrap();
        assert_eq!(json["width"], "fill");
        assert_eq!(json["height"], 120.0);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), image);
        assert!(serde_json::from_value::<Dimension>(serde_json::json!("cover")).is_err());
    }

    /// 滚动事件与新增布局字段：缺省字段反序列化照常（旧树不受影响）。
    #[test]
    fn scroll_event_and_layout_extensions_roundtrip() {
        let scroll = NodeBody::Scroll {
            direction: Some(Axis::Horizontal),
            gap: Some(8.0),
            height: Some(120.0),
        };
        let json = serde_json::to_value(&scroll).unwrap();
        assert_eq!(json["height"], 120.0);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), scroll);

        let grid = NodeBody::Grid {
            columns: Some(3),
            min_column_width: Some(180.0),
            gap: None,
        };
        assert_eq!(serde_json::to_value(&grid).unwrap()["minColumnWidth"], 180.0);
        assert_eq!(
            serde_json::from_value::<NodeBody>(serde_json::json!({ "kind": "grid" })).unwrap(),
            NodeBody::Grid { columns: None, min_column_width: None, gap: None }
        );

        let event = ViewEvent::Scroll { offset: 24.0, max_offset: 96.0 };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "scroll");
        assert_eq!(json["maxOffset"], 96.0);
        assert_eq!(serde_json::from_value::<ViewEvent>(json).unwrap(), event);
    }
}
