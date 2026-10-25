//! ViewNode 声明式控件树：插件提交树描述，宿主解释渲染并回发事件。
//! 控件集为封闭枚举（`kind` 标签），叶子节点不得携带 children；各控件共用的表述能力挂在节点或控件字段上。
//! 事件值按控件定型：文本 → `text`、数字输入 → `number`、勾选 → `bool`、下拉 → 下标、菜单条目 → `action`。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 一棵界面树。`id` 在同一棵树内必须唯一（宿主 diff 与事件回发都按它定位）；
/// `children` 仅容器类节点可携带（见 [`NodeBody::allows_children`]），`events` 声明该节点要回发哪些事件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewNode {
    pub id: String,
    #[serde(flatten)]
    pub body: NodeBody,
    /// 交互外观（见 [`Interaction`]）；缺省该节点只按自身控件渲染，不参与悬停 / 激活着色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction: Option<Interaction>,
    /// 在父级行 / 列 / 网格里按权重分剩余空间（缺省不参与分配，按控件自身尺寸）。
    /// 与兄弟的权重之比即分得空间之比（35 / 65 即写 35 与 65）；权重须大于 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grow: Option<u16>,
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

    /// 是否可成为键盘焦点：声明了 `focus` / `blur` / `key` 任一项。输入控件自己管键盘，恒不可聚焦。
    pub fn focusable(&self) -> bool {
        if matches!(self.body, NodeBody::TextInput { .. } | NodeBody::NumberInput { .. }) {
            return false;
        }
        self.events
            .iter()
            .any(|kind| matches!(kind, EventKind::Focus | EventKind::Blur | EventKind::Key))
    }

    /// 宿主挂载前的结构校验：id 非空且全树唯一、叶子无 children、浮层有且仅有一个内容子节点、
    /// 弹性权重为正、图标名非空、文本行数上限为正、容器尺寸不为负。
    pub fn validate(&self) -> Result<(), String> {
        if matches!(self.body, NodeBody::Overlay { .. }) {
            return Err("根节点不能是浮层：浮层须挂在某个容器下".into());
        }
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
        if matches!(self.body, NodeBody::Overlay { .. }) && self.children.len() != 1 {
            return Err(format!("浮层节点 {} 须有且仅有一个子节点（浮层内容）", self.id));
        }
        if self.grow == Some(0) {
            return Err(format!("节点 {} 的 grow 须大于 0", self.id));
        }
        if let NodeBody::Icon { name, .. } = &self.body {
            if name.trim().is_empty() {
                return Err(format!("图标节点 {} 的 name 不能为空", self.id));
            }
        }
        if let NodeBody::Text { max_lines: Some(0), .. } = &self.body {
            return Err(format!("文本节点 {} 的 maxLines 至少为 1", self.id));
        }
        if let Some(size) = self.body.size() {
            if let Some(slot) = size.negative_slot() {
                return Err(format!("节点 {} 的 {} 不能为负", self.id, slot));
            }
        }
        if let NodeBody::Canvas { fps, .. } = &self.body {
            if let Some(fps) = fps {
                if !(1..=crate::canvas::MAX_CANVAS_FPS).contains(fps) {
                    return Err(format!(
                        "画布节点 {} 的 fps 须在 1–{} 之间",
                        self.id,
                        crate::canvas::MAX_CANVAS_FPS
                    ));
                }
            }
        }
        // 输入控件自己承接键盘（光标 / 选区 / 组合串），焦点与按键声明会和控件内部处理互相截断
        if matches!(self.body, NodeBody::TextInput { .. } | NodeBody::NumberInput { .. }) {
            if let Some(kind) = self
                .events
                .iter()
                .find(|kind| matches!(kind, EventKind::Focus | EventKind::Blur | EventKind::Key))
            {
                let name = match kind {
                    EventKind::Focus => "focus",
                    EventKind::Blur => "blur",
                    _ => "key",
                };
                return Err(format!("输入控件 {} 不得声明 {name} 事件（键盘由控件自身承接）", self.id));
            }
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
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
        /// 主轴分布：`spaceBetween` 在行占满宽度时把子项推向两端（工具行：左信息 + 右操作）。
        /// 缺省 `start`（子项紧贴左缘）；声明 `size.width` 为 `fill` 时仍生效。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        justify: Option<Justify>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        /// 自身尺寸：缺省按内容收缩（有 `grow` 子项或声明 `justify` 时按占满宽度布局）；`width: fill` 即占满父容器。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    Column {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cross_align: Option<Align>,
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 自身尺寸：缺省宽度占满父容器、高度按内容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    /// 卡片容器：主题化背景 + 边框 + 圆角的分组盒（列表条目、信息分组）。
    /// 声明 `interaction` 且当前状态命中某个外观档时，底色与边框由该档提供，本控件自身不再绘制（单一底色来源）；
    /// 当前状态没有对应档位时仍按本控件的 `tone` / `accent` / `bordered` / `radius` 绘制。
    Card {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 内边距：数字 = 四边等距，也可按轴 / 按边声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding: Option<PaddingSpec>,
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
        /// 宽度不足父容器时的水平摆放（`center` 居中、`end` 右对齐）；缺省靠左。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 自身尺寸：缺省宽度占满父容器、高度按内容。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<BoxSize>,
    },
    /// 叠层：子节点按提交顺序重叠（后提交的在上），同尺寸对齐于左上角。
    /// 用于「自绘画布当底 + 宿主控件叠其上」或徽标叠图。
    Stack {},
    /// 面内浮层（模态）：唯一子节点即浮层内容，宿主把它铺满整个插件面——一层遮罩 + 居中内容，
    /// 且不参与父容器排版（挂在任何一层效果相同，浮层之间按树序叠放）。
    /// 打开 = 节点出现在树里，关闭 = 从树里移除；遮罩拦下所有指针事件（按下 / 抬起 / 滚轮），
    /// 下层控件不响应点击也不滚动。声明 `click` 时点击遮罩（内容之外）回发 `Click`，
    /// 插件据此收起浮层；不声明则遮罩只拦截。浮层自身不参与排版与交互外观（`size` / `grow` /
    /// `interaction` 不生效，外观取 `scrim` 与内容节点），键盘与焦点不被改写。浮层不能作根节点。
    Overlay {
        /// 遮罩底色（缺省主题遮罩；`transparent` 不画底色，只保留事件拦截）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scrim: Option<ColorSpec>,
    },
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
        /// 贴底跟随：用户停在终点时，内容增高后自动跟到新的终点；用户往回滚即脱钩，
        /// 再滚回终点自动恢复。`view.scrollTo(end)` 也会恢复跟随（其它位置则脱钩）。
        /// 缺省 false。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_end: Option<bool>,
    },
    /// 网格：`columns` 固定列数；`min_column_width` 按可用宽度换列（`columns` 同时给定时作列数上限）。
    /// 二者都缺省按单列渲染。`row_height` 缺省按列宽 1:1 定行高。
    Grid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        columns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_column_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 行高：数字 = 固定逻辑像素，`content` = 按单元格内容高度（短内容行不被拉成方格）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_height: Option<RowHeight>,
    },
    /// 可滚动列表：语义等同纵向 scroll + column（宿主对子项按键复用控件态）。
    /// 位置可用 `view.scrollTo` 指令控制；声明 `scroll` 事件即上报滚动位置。
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<f32>,
        /// 贴底跟随（语义同 [`NodeBody::Scroll`] 的 `followEnd`）；缺省 false。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_end: Option<bool>,
    },
    Text {
        content: String,
        /// 字号倍率（相对界面基准正文字号；缺省 1.0）。跟界面字号缩放，不是绝对像素。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<TextWeight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorSpec>,
        /// 水平对齐：声明后节点占满父容器并按该值摆放入文本；缺省按内容宽度收缩。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        align: Option<Align>,
        /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族（数值 / 路径 / 日志按位列对齐）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
        /// 是否允许按词换行；缺省 true。`false` 即单行（长标签不把行撑高）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wrap: Option<bool>,
        /// 最多渲染几行（至少 1）；超出时最后一行以省略号收尾。缺省不限。
        /// `wrap: false` 时恒单行，`maxLines` 只决定是否截断。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<u32>,
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
        color: Option<ColorSpec>,
        /// 字体族：`ui` 跟随界面字体（缺省），`mono` 用系统等宽族。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<TextFamily>,
    },
    TextInput {
        #[serde(default)]
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 多行：宿主按多行编辑器渲染。声明 `submit` 时 Enter 提交、Shift+Enter 换行；
        /// 未声明时 Enter 换行。单行恒按 Enter 提交。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multiline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<bool>,
        /// 只读：保留输入控件外观但不接受编辑，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
        /// 值上报档位（缺省逐键上报）。非逐键时输入值由宿主本地持有并显示，插件不必回显。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<ReportMode>,
    },
    NumberInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// 步进值（正数，缺省 1）：节点带 `change` 事件时，输入框右侧渲染上下步进按钮，
        /// 每次点击按该值增减，结果受 min / max 约束。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// 只读：保留输入控件外观但不接受编辑、无步进，不产生事件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
        /// 值上报档位（缺省逐键上报）。非逐键时输入值由宿主本地持有并显示，插件不必回显。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<ReportMode>,
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
        /// 前置图标（内置形状名或 `view.defineIcon` 登记的自备名）；缺省只有文字。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
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
    /// 纯装饰图标（不接受点击与悬停；空态、状态行、标题前的语义标记）。
    /// 需要可点的图标用 `iconButton`。
    Icon {
        /// 名字解析与 `iconButton` 同口径。
        name: String,
        /// 图标边长（px，缺省 16）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<ColorSpec>,
    },
    /// 图片：`width` / `height` 给定时按该尺寸渲染（数字 = px，`"fill"` = 撑满父容器该轴，
    /// `"shrink"` = 按原图；缺省按原图尺寸）；`fit` 决定尺寸内的适配方式，缺省 `contain`（不裁切不拉伸）。
    /// 节点声明 `click` 事件即成为可点击图片（大图预览入口）。
    Image {
        source: ImageSource,
        /// 尺寸内适配：`contain` 完整可见、`cover` 铺满并裁掉溢出、`fill` 拉伸铺满。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<ImageFit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<SizeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<SizeSpec>,
    },
    /// 进度条：0.0 ~ 1.0（越界值宿主钳制）。
    Progress {
        value: f64,
        /// 轨道粗细（px，缺省 30 = 系统标准粗条）；细进度条写 3 / 4。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        girth: Option<f32>,
        /// 完成段颜色（缺省主题强调色）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<ColorSpec>,
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
    /// `height` 给定时取该高度；`fill` = 占满父容器剩余空间（父容器可分配空间才有实际意义）。
    /// 画布尺寸变化时宿主回发 `host:canvas.resized`（插件据此对齐绘制坐标与滚动范围）。
    Canvas {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mouse_events: Vec<CanvasEventKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<bool>,
        /// 目标帧率（1–120）：声明后宿主按该节拍回发 `host:canvas.frame`，插件逐帧提交绘制内容；
        /// 缺省不订阅，静态画布不产生帧开销。不要求帧回调的插件不要声明。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u16>,
    },
    /// 分隔线：主题弱边界色的 1px 水平线（分组容器内的轻分割）。
    Divider {},
    /// 拖动热区：自身不渲染任何内容，用户在其占位区域内按下即由宿主发起整窗拖动
    /// （全接管或无系统边框的插件窗口里自绘标题栏的拖动把手）。事件不回发。
    DragRegion {},
}

impl NodeBody {
    /// 容器类节点（可携带 children）：布局容器、浮层、滚动容器、网格与列表。
    pub fn allows_children(&self) -> bool {
        matches!(
            self,
            Self::Row { .. }
                | Self::Column { .. }
                | Self::Card { .. }
                | Self::Stack { .. }
                | Self::Overlay { .. }
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
            Self::Overlay { .. } => "overlay",
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
            Self::Icon { .. } => "icon",
            Self::Image { .. } => "image",
            Self::Progress { .. } => "progress",
            Self::Menu { .. } => "menu",
            Self::Canvas { .. } => "canvas",
            Self::Divider { .. } => "divider",
            Self::DragRegion { .. } => "dragRegion",
        }
    }

    /// 容器类节点声明的自身尺寸（其余控件的尺寸由控件自身语义决定）。
    pub fn size(&self) -> Option<&BoxSize> {
        match self {
            Self::Row { size, .. } | Self::Column { size, .. } | Self::Card { size, .. } => {
                size.as_ref()
            }
            _ => None,
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

/// 单轴尺寸：`fill` 占满父容器该轴、`shrink` 按内容收缩、数字为固定逻辑像素。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizeSpec {
    Fill,
    Shrink,
    Fixed(f32),
}

impl SizeSpec {
    /// 固定像素值（其余形态为 `None`）。
    pub fn fixed(self) -> Option<f32> {
        match self {
            Self::Fixed(value) => Some(value),
            _ => None,
        }
    }
}

impl Serialize for SizeSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fill => serializer.serialize_str("fill"),
            Self::Shrink => serializer.serialize_str("shrink"),
            Self::Fixed(value) => serializer.serialize_f32(*value),
        }
    }
}

impl<'de> Deserialize<'de> for SizeSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SizeVisitor;

        impl<'de> serde::de::Visitor<'de> for SizeVisitor {
            type Value = SizeSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("像素数字或 \"fill\" / \"shrink\"")
            }

            fn visit_f32<E: serde::de::Error>(self, value: f32) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Fixed(value))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Fixed(value as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Fixed(value as f32))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Fixed(value as f32))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<SizeSpec, E> {
                match value {
                    "fill" => Ok(SizeSpec::Fill),
                    "shrink" => Ok(SizeSpec::Shrink),
                    other => Err(E::unknown_variant(other, &["fill", "shrink"])),
                }
            }
        }

        deserializer.deserialize_any(SizeVisitor)
    }
}

/// 容器自身尺寸与上下限（`Row` / `Column` / `Card` 的 `size`）。
/// 上下限只对固定或收缩尺寸的子项生效：流体（`fill` 或父级按 `grow` 分配）子项的空间由父容器分配，
/// 此时上下限不参与布局，需要约束空间用父级的 `grow` 权重。最小尺寸抬的是节点外框，内容仍按自身
/// 尺寸贴左上摆放（要让内容一起撑开，把内容声明为 `fill`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoxSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<SizeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<SizeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<f32>,
}

impl BoxSize {
    /// 第一个为负的字段名（宿主校验用；尺寸与上下限都不接受负值）。
    pub fn negative_slot(&self) -> Option<&'static str> {
        let negative = |value: &Option<f32>| value.is_some_and(|value| value < 0.0);
        if self.width.and_then(SizeSpec::fixed).is_some_and(|value| value < 0.0) {
            return Some("width");
        }
        if self.height.and_then(SizeSpec::fixed).is_some_and(|value| value < 0.0) {
            return Some("height");
        }
        if negative(&self.min_width) {
            return Some("minWidth");
        }
        if negative(&self.max_width) {
            return Some("maxWidth");
        }
        if negative(&self.min_height) {
            return Some("minHeight");
        }
        if negative(&self.max_height) {
            return Some("maxHeight");
        }
        None
    }
}

/// 网格行高（[`NodeBody::Grid`] 的 `row_height`）：数字 = 固定逻辑像素，`content` = 按内容。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowHeight {
    Fixed(f32),
    Content,
}

impl Serialize for RowHeight {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(value) => serializer.serialize_f32(*value),
            Self::Content => serializer.serialize_str("content"),
        }
    }
}

impl<'de> Deserialize<'de> for RowHeight {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowHeightVisitor;

        impl<'de> serde::de::Visitor<'de> for RowHeightVisitor {
            type Value = RowHeight;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("像素数字或 \"content\"")
            }

            fn visit_f32<E: serde::de::Error>(self, value: f32) -> Result<RowHeight, E> {
                Ok(RowHeight::Fixed(value))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<RowHeight, E> {
                Ok(RowHeight::Fixed(value as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<RowHeight, E> {
                Ok(RowHeight::Fixed(value as f32))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<RowHeight, E> {
                Ok(RowHeight::Fixed(value as f32))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<RowHeight, E> {
                match value {
                    "content" => Ok(RowHeight::Content),
                    other => Err(E::unknown_variant(other, &["content"])),
                }
            }
        }

        deserializer.deserialize_any(RowHeightVisitor)
    }
}

/// 交互外观（节点的 `interaction`）：决定该节点的命中与状态着色。
/// 交互态按「禁用 > 激活 > 焦点 > 悬停 > 常态」取第一个已声明且命中的档位；该档未声明时继续往后找，最终落到常态（连常态也没有则不绘制外观）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Interaction {
    /// 当前状态：缺省 `normal`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<InteractionState>,
    /// 常态外观。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<Surface>,
    /// 悬停外观（鼠标进入节点矩形）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover: Option<Surface>,
    /// 键盘焦点外观（节点持有键盘焦点时；键盘用户据此看见当前目标）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<Surface>,
    /// 激活外观（列表选中项、当前 tab）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<Surface>,
    /// 禁用外观（不接受点击，也不响应悬停）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Surface>,
}

impl Interaction {
    /// 节点当前状态（缺省 `normal`）。
    pub fn state(&self) -> InteractionState {
        self.state.unwrap_or(InteractionState::Normal)
    }

    /// 是否接受点击与悬停（`disabled` 两者都不接受）。
    pub fn interactive(&self) -> bool {
        self.state() != InteractionState::Disabled
    }

    /// 当前应绘制的外观：禁用 / 激活 / 焦点 / 悬停 / 常态依次取第一个命中的档位。
    /// 激活优先于焦点——选中项持有焦点时仍保留选中底色与描边；焦点优先于悬停。
    pub fn appearance(&self, hovered: bool, focused: bool) -> Option<&Surface> {
        if !self.interactive() {
            return self.disabled.as_ref().or(self.normal.as_ref());
        }
        if self.state() == InteractionState::Active {
            if let Some(surface) = &self.active {
                return Some(surface);
            }
        }
        if focused {
            if let Some(surface) = &self.focused {
                return Some(surface);
            }
        }
        if hovered {
            if let Some(surface) = &self.hover {
                return Some(surface);
            }
        }
        self.normal.as_ref()
    }
}

/// 交互状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InteractionState {
    Normal,
    Active,
    /// 不接受点击与悬停，可选声明 `disabled` 外观。
    Disabled,
}

/// 一层外观：底色 + 边框色 + 圆角。三者都可省，只声明的项生效。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Surface {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<ColorSpec>,
    /// 边框色（1px 描边）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<ColorSpec>,
    /// 圆角半径（px）；缺省用宿主小圆角。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
}

/// 取色：引用宿主主题 token，或给 `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa` 自定义色。
/// 主题 token 随主题与字号设置联动；自定义色由插件自负对比度。未知 token 与非法色值都拒收。
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ColorSpec {
    Token(ColorToken),
    Custom(Rgba),
}

impl<'de> Deserialize<'de> for ColorSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::IntoDeserializer;

        let text = String::deserialize(deserializer)?;
        if text.trim_start().starts_with('#') {
            return Rgba::parse(&text)
                .map(Self::Custom)
                .map_err(serde::de::Error::custom);
        }
        ColorToken::deserialize(text.as_str().into_deserializer()).map(Self::Token)
    }
}

/// 自定义色（sRGB，各通道 0.0–1.0）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl From<ColorToken> for ColorSpec {
    fn from(token: ColorToken) -> Self {
        Self::Token(token)
    }
}

impl Rgba {
    /// 解析 `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa`（`#` 可省）。
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex = text.trim().trim_start_matches('#');
        // 先确认全是 ASCII 十六进制字符：非 ASCII 串按字节切片会落在字符边界外
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("颜色 {text} 不是合法的十六进制色值"));
        }
        let digit = |index: usize, len: usize| -> f32 {
            let value = u8::from_str_radix(&hex[index..index + len], 16).expect("已校验为十六进制");
            if len == 1 {
                // `#abc` 的每位重复一次（f → ff）
                (value * 17) as f32 / 255.0
            } else {
                value as f32 / 255.0
            }
        };
        let mut channels = [1.0; 4];
        let digits = match hex.len() {
            3 | 4 => 1,
            6 | 8 => 2,
            _ => return Err(format!("颜色 {text} 须为 #rgb / #rgba / #rrggbb / #rrggbbaa")),
        };
        for (index, channel) in channels.iter_mut().enumerate().take(hex.len() / digits) {
            *channel = digit(index * digits, digits);
        }
        Ok(Self(channels))
    }

    /// 编码回 `#rrggbb`（不透明）或 `#rrggbbaa`。
    pub fn to_hex(self) -> String {
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        let [r, g, b, a] = self.0.map(byte);
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }
}

impl Serialize for Rgba {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

/// 字体族：界面字体（跟随用户设置）或系统等宽族。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextFamily {
    Ui,
    Mono,
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

/// 主题 token：取色引用宿主调色板，主题与字号设置变化时自动跟随。
/// `accentSoft` / `warningSoft` / `dangerSoft` / `successSoft` 为对应语义色的低饱和底（提示块、徽标），
/// `surface*` / `hover` / `border*` 为层次底色与边界（分组盒、分隔、悬停反馈）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorToken {
    Text,
    TextSecondary,
    TextMuted,
    Accent,
    AccentSoft,
    Danger,
    DangerSoft,
    Success,
    SuccessSoft,
    Warning,
    WarningSoft,
    /// 内嵌表面（比面板底色深一档：输入框、内嵌卡片）。
    Surface,
    /// 次级表面（面板头 / 工具条底）。
    SurfaceMuted,
    /// 最深表面（内陷区域）。
    SurfaceSunken,
    /// 悬停反馈底色。
    Hover,
    Border,
    BorderStrong,
    /// 无底色 / 无边框。
    Transparent,
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
/// 协作空间走同构内容面）；`url` = http(s) 地址（缺省按公网策略下载并缓存，`local` 改为本机 /
/// 局域网策略）；`file` = 本机绝对路径（插件自己读得到的文件，交给宿主显示）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImageSource {
    Private { path: String },
    Vault { path: String },
    Url {
        url: String,
        /// 按本机 / 局域网策略取图：放行回环与私网、仅拒链路本地与广播（插件连接本机服务时用）。
        /// 缺省 = 公网策略（拒回环与私网）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local: Option<bool>,
    },
    /// 本机绝对路径。渲染层的图源不做权限门禁（与 `private` / `vault` 同口径），宿主按
    /// 原路径读盘显示；插件要读它的内容本来就能经 `fs` 自己读，这里不增加也不收紧范围。
    File { path: String },
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
    /// 分组名（单层）：相邻同名条目归入同一组，宿主在组首画一次组标题；无分组条目独立成行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: action.into(),
            disabled: None,
            danger: None,
            current: None,
            group: None,
        }
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

    /// 归入一个分组（相邻同名条目共用组标题）。
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }
}

/// 输入控件的上报档位：决定宿主何时把用户输入回发给插件。
///
/// `Immediate` 下宿主每次编辑都回发 `change`，插件须把值写回树——宿主用「已上报值 = 插件值」
/// 判定能否显示本机文本，不回写即表现为输入不显示。`Commit` / `Debounced` 下输入值完全由
/// 宿主本地持有并显示，插件不必回显；宿主仅在插件下发的值发生变化时视为主动改值、接管输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReportMode {
    /// 逐键上报。
    Immediate,
    /// 提交时上报：单行按 Enter；多行在停止输入后上报，声明 `submit` 时按 Enter 立即上报
    /// （Enter 另发 `submit`，Shift+Enter 换行）。
    Commit,
    /// 停顿上报：停止编辑 `Debounced` 毫秒后回发 `change`（毫秒为 0 按 [`ReportMode::Immediate`] 处理）。
    Debounced(u32),
}

impl ReportMode {
    /// 是否由宿主本地持有输入值（不逐键回发）。停顿时长为 0 按逐键处理——
    /// 否则既不逐键回发、也不会有到期上报，输入永远到不了插件。
    pub fn is_deferred(self) -> bool {
        match self {
            ReportMode::Immediate => false,
            ReportMode::Commit => true,
            ReportMode::Debounced(ms) => ms > 0,
        }
    }
}

/// 节点事件种类：声明在 [`ViewNode::events`] 里宿主才回发，未声明的事件不上行（省流量、省误触）。
///
/// 声明 `focus` / `blur` / `key` 任一项即把节点标为**可聚焦**：在该节点上按下即取得键盘焦点，
/// 在别处按下即失去；同一面内至多一个节点持有焦点，同一窗口同一时刻至多一个面接收键盘。
/// 输入类控件（单行 / 多行 / 数字）自己管键盘，不得声明 `key`（提交时拒收）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Click,
    /// 右键在节点矩形内按下（与 `click` 相互独立，可同时声明）。
    /// 画布节点自带带按键的指针事件，右键用 `mouseEvents` 的 `pointerDown` 即可；两者都声明时
    /// 一次右键会各回发一次。
    SecondaryClick,
    Change,
    Submit,
    /// 取得键盘焦点（仅可聚焦节点）。
    Focus,
    /// 失去键盘焦点（仅可聚焦节点）。
    Blur,
    /// 键盘按键（仅可聚焦节点）：持焦点期间回发全部**无命令修饰**的按键，带 Ctrl / Alt / Super
    /// 的组合键归宿主、不上行。按键本身不做逐键声明——需要更细的筛选由插件在回调里判。
    Key,
    /// Markdown 正文内的链接点击（仅 `markdown` 节点；宿主默认打开行为不受声明影响）。
    LinkClick,
    /// 滚动位置变化（仅 `scroll` / `list` 节点）：滚动时回发当前偏移与可滚动余量；
    /// 内容不足一屏时没有滚动，不发事件。
    Scroll,
}

/// 键盘焦点事件与按键的归属：`Press` 含系统重复按键（长按方向键逐次到达），`Release` 在抬起时到达。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyAction {
    Press,
    Release,
}

/// 按键：具名键（功能键与编辑键）或可打印字符。字符按布局产出（与用户所见一致），
/// 大小写与符号跟随 Shift，因此 `a` 与 `A` 是两个值、`1` 与 `!` 也是两个值。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Key {
    Named { name: NamedKey },
    Character { text: String },
}

/// 具名键。可打印字符走 [`Key::Character`]；这里放不可打印的编辑 / 功能键，外加空格
/// （它与编辑语义绑定，且布局差异下字符形态不稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// 修饰键状态（按键发生时的快照）：`control` = Ctrl（macOS 的 Control，不含 Command），
/// `meta` = Windows / Super / Command 键。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Modifiers {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meta: bool,
}


/// 宿主回发给插件的事件负载。`change`/`submit` 携带新值；菜单条目选择复用 `change`，
/// 值为条目的 `action` 文本（[`EventValue::Text`]）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum ViewEvent {
    Click,
    SecondaryClick,
    Change { value: EventValue },
    Submit { value: EventValue },
    Focus,
    Blur,
    /// 键盘按键（仅声明 `key` 的可聚焦节点）：`key` 为按键本身，`action` 区分按下 / 抬起，
    /// `modifiers` 为当时的修饰键快照。
    Key { key: Key, action: KeyAction, modifiers: Modifiers },
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

    fn leaf(id: &str, body: NodeBody) -> ViewNode {
        ViewNode { id: id.into(), body, interaction: None, grow: None, children: vec![], events: vec![] }
    }

    fn sample_tree() -> ViewNode {
        let mut root = leaf(
            "root",
            NodeBody::Column {
                gap: Some(8.0),
                padding: Some(PaddingSpec::All(12.0)),
                cross_align: Some(Align::Start),
                align: None,
                size: None,
            },
        );
        let mut query = leaf(
            "query",
            NodeBody::TextInput {
                value: "示例文本".into(),
                placeholder: Some("输入…".into()),
                multiline: None,
                password: None,
                read_only: None,
                report: None,
            },
        );
        query.events = vec![EventKind::Submit];
        let mut go = leaf(
            "go",
            NodeBody::Button {
                label: "开始".into(),
                icon: None,
                variant: Some(ButtonVariant::Primary),
                enabled: None,
                size: None,
            },
        );
        go.events = vec![EventKind::Click];
        root.children = vec![query, go];
        root
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

    /// 停顿时长为 0 必须按逐键处理，否则输入既不逐键回发、也不会有到期上报。
    #[test]
    fn zero_debounce_falls_back_to_immediate() {
        assert!(!ReportMode::Debounced(0).is_deferred());
        assert!(ReportMode::Debounced(1).is_deferred());
        assert!(ReportMode::Commit.is_deferred());
        assert!(!ReportMode::Immediate.is_deferred());
    }

    /// 上报档位的编码形状是对外契约：改形状即改协议。
    #[test]
    fn report_mode_encoding_is_frozen() {
        assert_eq!(serde_json::to_value(ReportMode::Immediate).unwrap(), serde_json::json!("immediate"));
        assert_eq!(serde_json::to_value(ReportMode::Commit).unwrap(), serde_json::json!("commit"));
        assert_eq!(
            serde_json::to_value(ReportMode::Debounced(200)).unwrap(),
            serde_json::json!({ "debounced": 200 })
        );
        let body = NodeBody::TextInput {
            value: "x".into(),
            placeholder: None,
            multiline: None,
            password: None,
            read_only: None,
            report: Some(ReportMode::Commit),
        };
        assert_eq!(serde_json::to_value(&body).unwrap()["report"], serde_json::json!("commit"));
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

        let mut with_children = sample_tree();
        with_children.children[1].children = vec![with_children.children[0].clone()];
        assert!(with_children.validate().unwrap_err().contains("不得携带 children"));
    }

    #[test]
    fn validate_rejects_zero_grow_and_empty_icon_name() {
        let mut tree = sample_tree();
        tree.children[0].grow = Some(0);
        assert!(tree.validate().unwrap_err().contains("grow"));

        let mut tree = sample_tree();
        tree.children[0] = leaf("icon", NodeBody::Icon { name: "  ".into(), size: None, color: None });
        assert!(tree.validate().unwrap_err().contains("name"));
    }

    #[test]
    fn validate_rejects_negative_size() {
        let mut tree = sample_tree();
        tree.children[0] = leaf(
            "box",
            NodeBody::Card {
                gap: None,
                padding: None,
                accent: None,
                tone: None,
                radius: None,
                bordered: None,
                align: None,
                size: Some(BoxSize { min_width: Some(-1.0), ..BoxSize::default() }),
            },
        );
        assert!(tree.validate().unwrap_err().contains("minWidth"));
    }

    #[test]
    fn validate_rejects_zero_max_lines() {
        let mut tree = sample_tree();
        tree.children.push(leaf(
            "label",
            NodeBody::Text {
                content: "长标签".into(),
                size: None,
                weight: None,
                color: None,
                align: None,
                family: None,
                wrap: None,
                max_lines: Some(0),
            },
        ));
        assert!(tree.validate().unwrap_err().contains("maxLines"));
    }

    /// 尺寸三形态与上下限：关键字与像素都落到 `SizeSpec`，拼错的字段名拒收。
    #[test]
    fn size_spec_and_box_size_roundtrip() {
        let box_size = BoxSize {
            width: Some(SizeSpec::Fixed(320.0)),
            height: Some(SizeSpec::Shrink),
            min_width: Some(120.0),
            max_width: Some(600.0),
            min_height: Some(48.0),
            max_height: Some(240.0),
        };
        let node = NodeBody::Card {
            gap: None,
            padding: None,
            accent: None,
            tone: None,
            radius: None,
            bordered: None,
            align: Some(Align::Center),
            size: Some(box_size),
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["size"]["width"], 320.0);
        assert_eq!(json["size"]["height"], "shrink");
        assert_eq!(json["size"]["minHeight"], 48.0);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), node);

        // 未声明 size 的树照常反序列化，行为按控件缺省
        let plain: NodeBody = serde_json::from_value(serde_json::json!({ "kind": "row" })).unwrap();
        assert_eq!(
            plain,
            NodeBody::Row { gap: None, padding: None, justify: None, cross_align: None, size: None }
        );
        assert!(serde_json::from_value::<SizeSpec>(serde_json::json!("cover")).is_err());
        assert!(serde_json::from_value::<BoxSize>(serde_json::json!({ "minWidht": 10.0 })).is_err());
    }

    /// 交互外观取档：禁用 > 激活 > 悬停 > 常态，未声明该档沿用常态。
    #[test]
    fn interaction_appearance_prefers_disabled_then_active_then_hover() {
        let normal = Surface {
            background: Some(ColorSpec::Token(ColorToken::Surface)),
            border: None,
            radius: None,
        };
        let hover = Surface {
            background: Some(ColorSpec::Token(ColorToken::Hover)),
            border: None,
            radius: None,
        };
        let active = Surface {
            background: Some(ColorSpec::Token(ColorToken::AccentSoft)),
            border: Some(ColorSpec::Token(ColorToken::Accent)),
            radius: None,
        };
        let spec = Interaction {
            state: Some(InteractionState::Active),
            normal: Some(normal),
            hover: Some(hover),
            focused: None,
            active: Some(active),
            disabled: None,
        };
        assert_eq!(spec.appearance(false, false), Some(&active));
        // 激活优先于悬停：选中项在指针下仍保留选中外观
        assert_eq!(spec.appearance(true, false), Some(&active));

        // 未激活时悬停取悬停档
        let plain = Interaction { state: None, ..spec.clone() };
        assert_eq!(plain.appearance(false, false), Some(&normal));
        assert_eq!(plain.appearance(true, false), Some(&hover));

        // 未声明悬停档：悬停时沿用常态
        let no_hover = Interaction { state: None, hover: None, ..spec.clone() };
        assert_eq!(no_hover.appearance(true, false), Some(&normal));

        // 禁用：不接受交互，外观取禁用档（缺省回落常态）
        let disabled = Interaction { state: Some(InteractionState::Disabled), ..spec.clone() };
        assert!(!disabled.interactive());
        assert_eq!(disabled.appearance(true, false), Some(&normal));
        let disabled_styled = Interaction { disabled: Some(hover), ..disabled };
        assert_eq!(disabled_styled.appearance(true, false), Some(&hover));
    }

    #[test]
    fn interaction_json_roundtrips_with_token_and_custom_color() {
        let mut node = leaf(
            "row",
            NodeBody::Row { gap: None, padding: None, justify: None, cross_align: None, size: None },
        );
        node.interaction = Some(Interaction {
            state: Some(InteractionState::Active),
            normal: Some(Surface {
                background: Some(ColorSpec::Token(ColorToken::Surface)),
                border: Some(ColorSpec::Custom(Rgba([
                    26.0 / 255.0,
                    51.0 / 255.0,
                    77.0 / 255.0,
                    128.0 / 255.0,
                ]))),
                radius: Some(6.0),
            }),
            hover: None,
            focused: None,
            active: None,
            disabled: None,
        });
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["interaction"]["state"], "active");
        assert_eq!(json["interaction"]["normal"]["background"], "surface");
        assert_eq!(json["interaction"]["normal"]["border"], "#1a334d80");
        assert_eq!(serde_json::from_value::<ViewNode>(json).unwrap(), node);
    }

    #[test]
    fn custom_color_accepts_hex_forms_and_rejects_others() {
        assert_eq!(Rgba::parse("#abc").unwrap().to_hex(), "#aabbcc");
        assert_eq!(Rgba::parse("aabbcc").unwrap().to_hex(), "#aabbcc");
        assert_eq!(Rgba::parse("#AABBCCDD").unwrap().to_hex(), "#aabbccdd");
        assert!(Rgba::parse("#abcd").unwrap().to_hex() == "#aabbccdd");
        assert!(Rgba::parse("#ab").is_err());
        assert!(Rgba::parse("#gggggg").is_err());
        // 非 ASCII 串按字节切片会落在字符边界外：必须在切片前拒收
        assert!(Rgba::parse("€").is_err());
        assert!(Rgba::parse("##€€").is_err());
        assert!(Rgba::parse("#+a0000").is_err());
        // 未知 token 与非法色值都拒收，不给「写错了静默变默认色」的机会
        assert!(serde_json::from_value::<ColorSpec>(serde_json::json!("textPrimay")).is_err());
        assert!(serde_json::from_value::<ColorSpec>(serde_json::json!("#12345")).is_err());
    }

    /// 行主轴两端对齐与文本水平对齐走独立枚举（不与交叉轴 Align 混用）。
    #[test]
    fn justify_and_card_tone_roundtrip() {
        let row = NodeBody::Row {
            gap: None,
            padding: None,
            justify: Some(Justify::SpaceBetween),
            cross_align: None,
            size: Some(BoxSize { width: Some(SizeSpec::Fill), ..BoxSize::default() }),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["justify"], "spaceBetween");
        assert_eq!(json["size"]["width"], "fill");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), row);

        let text = NodeBody::Text {
            content: "右对齐".into(),
            size: Some(1.25),
            weight: None,
            color: None,
            align: Some(Align::End),
            family: None,
            wrap: None,
            max_lines: None,
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
            align: None,
            size: None,
        };
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["tone"], "accentSoft");
        assert_eq!(json["bordered"], false);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), card);
    }

    /// 排版字段：字体族、单行不换行、行数上限、自定义取色。
    #[test]
    fn typography_fields_roundtrip() {
        let text = NodeBody::Text {
            content: "1.2 GB/s".into(),
            size: None,
            weight: Some(TextWeight::SemiBold),
            color: Some(ColorSpec::Token(ColorToken::WarningSoft)),
            align: None,
            family: Some(TextFamily::Mono),
            wrap: Some(false),
            max_lines: Some(1),
        };
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(json["family"], "mono");
        assert_eq!(json["wrap"], false);
        assert_eq!(json["maxLines"], 1);
        assert_eq!(json["color"], "warningSoft");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), text);

        let markdown = NodeBody::Markdown {
            content: "# 标题".into(),
            size: None,
            weight: None,
            color: None,
            family: Some(TextFamily::Ui),
        };
        let json = serde_json::to_value(&markdown).unwrap();
        assert_eq!(json["family"], "ui");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), markdown);
    }

    /// 装饰图标是叶子；按钮前置图标与进度条粗细、取色都可声明。
    #[test]
    fn icon_button_icon_and_progress_fields_roundtrip() {
        let icon = NodeBody::Icon {
            name: "circle-check".into(),
            size: Some(20.0),
            color: Some(ColorSpec::Token(ColorToken::Success)),
        };
        assert!(!icon.allows_children());
        assert_eq!(icon.kind_name(), "icon");
        let json = serde_json::to_value(&icon).unwrap();
        assert_eq!(json["name"], "circle-check");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), icon);

        let button = NodeBody::Button {
            label: "重载".into(),
            icon: Some("refresh".into()),
            variant: None,
            enabled: None,
            size: None,
        };
        let json = serde_json::to_value(&button).unwrap();
        assert_eq!(json["icon"], "refresh");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), button);

        let progress = NodeBody::Progress {
            value: 0.4,
            girth: Some(4.0),
            tone: Some(ColorSpec::Token(ColorToken::Accent)),
        };
        let json = serde_json::to_value(&progress).unwrap();
        assert_eq!(json["girth"], 4.0);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), progress);
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
            interaction: None,
            grow: None,
            children: vec![leaf("d", divider)],
            events: vec![],
        };
        assert!(with_children.validate().is_ok());
    }

    /// 浮层：唯一子节点为浮层内容；不能作根节点。
    #[test]
    fn overlay_requires_exactly_one_content_child() {
        let body = NodeBody::Overlay { scrim: Some(ColorSpec::Token(ColorToken::SurfaceSunken)) };
        assert!(body.allows_children());
        assert_eq!(body.kind_name(), "overlay");
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({ "kind": "overlay", "scrim": "surfaceSunken" })
        );
        assert_eq!(
            serde_json::from_value::<NodeBody>(serde_json::json!({ "kind": "overlay" })).unwrap(),
            NodeBody::Overlay { scrim: None }
        );

        let root = |children: Vec<ViewNode>| ViewNode {
            id: "root".into(),
            body: NodeBody::Column { gap: None, padding: None, cross_align: None, align: None, size: None },
            interaction: None,
            grow: None,
            children,
            events: vec![],
        };
        let layer = |id: &str, children: Vec<ViewNode>| ViewNode {
            id: id.into(),
            body: NodeBody::Overlay { scrim: None },
            interaction: None,
            grow: None,
            children,
            events: vec![],
        };
        let content = leaf("dialog", NodeBody::Divider {});

        assert!(root(vec![layer("layer", vec![content.clone()])]).validate().is_ok());
        assert!(root(vec![layer("layer", vec![])]).validate().unwrap_err().contains("仅有一个"));
        assert!(root(vec![layer("layer", vec![content.clone(), content.clone()])])
            .validate()
            .unwrap_err()
            .contains("仅有一个"));
        // 根节点自身是浮层：没有可承载的内容面，拒收
        assert!(layer("layer", vec![content.clone()]).validate().unwrap_err().contains("根节点"));
    }

    #[test]
    fn canvas_event_kind_matches_only_its_payload() {
        use CanvasEventKind::*;
        let down = CanvasPointer::Down { x: 1.0, y: 2.0, button: MouseButton::Left };
        assert!(PointerDown.matches(&down));
        assert!(!PointerUp.matches(&down));
        assert!(!Wheel.matches(&down));
        assert!(Wheel.matches(&CanvasPointer::Wheel { x: 0.0, y: 0.0, delta_x: 0.0, delta_y: 1.0 }));
        assert!(DoubleClick.matches(&CanvasPointer::DoubleClick {
            x: 0.0,
            y: 0.0,
            button: MouseButton::Right
        }));
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
    fn image_size_accepts_pixels_and_keywords() {
        let image = NodeBody::Image {
            source: ImageSource::Vault { path: "a.png".into() },
            fit: Some(ImageFit::Contain),
            width: Some(SizeSpec::Fill),
            height: Some(SizeSpec::Fixed(120.0)),
        };
        let json = serde_json::to_value(&image).unwrap();
        assert_eq!(json["width"], "fill");
        assert_eq!(json["height"], 120.0);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), image);
    }

    /// 滚动事件与布局扩展字段：缺省字段反序列化照常。
    #[test]
    fn scroll_event_and_layout_extensions_roundtrip() {
        let scroll = NodeBody::Scroll {
            direction: Some(Axis::Horizontal),
            gap: Some(8.0),
            height: Some(120.0),
            follow_end: Some(true),
        };
        let json = serde_json::to_value(&scroll).unwrap();
        assert_eq!(json["height"], 120.0);
        assert_eq!(json["followEnd"], true);
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), scroll);

        let grid = NodeBody::Grid {
            columns: Some(3),
            min_column_width: Some(180.0),
            gap: None,
            row_height: Some(RowHeight::Content),
        };
        let json = serde_json::to_value(&grid).unwrap();
        assert_eq!(json["minColumnWidth"], 180.0);
        assert_eq!(json["rowHeight"], "content");
        assert_eq!(serde_json::from_value::<NodeBody>(json).unwrap(), grid);
        assert_eq!(
            serde_json::from_value::<NodeBody>(serde_json::json!({ "kind": "grid" })).unwrap(),
            NodeBody::Grid { columns: None, min_column_width: None, gap: None, row_height: None }
        );

        let event = ViewEvent::Scroll { offset: 24.0, max_offset: 96.0 };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "scroll");
        assert_eq!(json["maxOffset"], 96.0);
        assert_eq!(serde_json::from_value::<ViewEvent>(json).unwrap(), event);
    }

    #[test]
    fn image_source_variants_carry_their_locator() {
        for (source, wire) in [
            (
                ImageSource::Private { path: "a.png".into() },
                serde_json::json!({ "type": "private", "path": "a.png" }),
            ),
            (
                ImageSource::Vault { path: "b.png".into() },
                serde_json::json!({ "type": "vault", "path": "b.png" }),
            ),
            (
                ImageSource::Url { url: "https://example.com/c.png".into(), local: None },
                serde_json::json!({ "type": "url", "url": "https://example.com/c.png" }),
            ),
            (
                ImageSource::Url { url: "http://127.0.0.1:8188/view".into(), local: Some(true) },
                serde_json::json!({ "type": "url", "url": "http://127.0.0.1:8188/view", "local": true }),
            ),
            (
                ImageSource::File { path: "/tmp/d.png".into() },
                serde_json::json!({ "type": "file", "path": "/tmp/d.png" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&source).unwrap(), wire);
            assert_eq!(serde_json::from_value::<ImageSource>(wire).unwrap(), source);
        }
    }

    #[test]
    fn secondary_click_and_key_events_roundtrip() {
        assert_eq!(serde_json::to_value(EventKind::SecondaryClick).unwrap(), serde_json::json!("secondaryClick"));
        assert_eq!(serde_json::to_value(EventKind::Key).unwrap(), serde_json::json!("key"));

        assert_eq!(serde_json::to_value(ViewEvent::SecondaryClick).unwrap(), serde_json::json!({ "kind": "secondaryClick" }));

        let named = ViewEvent::Key {
            key: Key::Named { name: NamedKey::ArrowLeft },
            action: KeyAction::Press,
            modifiers: Modifiers { shift: true, ..Modifiers::default() },
        };
        let wire = serde_json::to_value(&named).unwrap();
        assert_eq!(wire["kind"], "key");
        assert_eq!(wire["key"], serde_json::json!({ "type": "named", "name": "arrowLeft" }));
        assert_eq!(wire["action"], "press");
        assert_eq!(wire["modifiers"], serde_json::json!({ "shift": true }), "未按下的修饰键不落字段");
        assert_eq!(serde_json::from_value::<ViewEvent>(wire).unwrap(), named);

        let character = ViewEvent::Key {
            key: Key::Character { text: "A".into() },
            action: KeyAction::Release,
            modifiers: Modifiers::default(),
        };
        let wire = serde_json::to_value(&character).unwrap();
        assert_eq!(wire["key"], serde_json::json!({ "type": "character", "text": "A" }));
        assert_eq!(wire["modifiers"], serde_json::json!({}));
        assert_eq!(serde_json::from_value::<ViewEvent>(wire).unwrap(), character);
    }

    #[test]
    fn focused_appearance_sits_between_active_and_hover() {
        let focused = Surface { background: Some(ColorSpec::Token(ColorToken::Accent)), ..Surface::default() };
        let hover = Surface { background: Some(ColorSpec::Token(ColorToken::Surface)), ..Surface::default() };
        let spec = Interaction {
            state: None,
            normal: None,
            hover: Some(hover),
            focused: Some(focused),
            active: None,
            disabled: None,
        };
        assert_eq!(spec.appearance(false, true), Some(&focused));
        assert_eq!(spec.appearance(true, true), Some(&focused), "焦点优先于悬停");
        assert_eq!(spec.appearance(true, false), Some(&hover));

        let without_focus = Interaction { focused: None, ..spec.clone() };
        assert_eq!(without_focus.appearance(true, true), Some(&hover), "未声明焦点档时落到悬停档");

        let wire = serde_json::to_value(&spec).unwrap();
        assert!(wire.get("focused").is_some());
        assert_eq!(serde_json::from_value::<Interaction>(wire).unwrap(), spec);
    }

    #[test]
    fn focusable_is_declared_by_events_and_refused_on_inputs() {
        let mut node = leaf("board", NodeBody::Canvas { mouse_events: vec![], height: None, fill: None, fps: None });
        assert!(!node.focusable());
        node.events = vec![EventKind::Click];
        assert!(!node.focusable(), "只声明 click 不算可聚焦");
        node.events = vec![EventKind::Key];
        assert!(node.focusable());
        node.events = vec![EventKind::Focus];
        assert!(node.focusable());

        let mut input = leaf(
            "query",
            NodeBody::TextInput {
                value: String::new(),
                placeholder: None,
                multiline: None,
                password: None,
                read_only: None,
                report: None,
            },
        );
        input.events = vec![EventKind::Key];
        assert!(!input.focusable());
        assert!(input.validate().unwrap_err().contains("不得声明 key"));
    }

    #[test]
    fn canvas_fps_out_of_range_is_refused() {
        for (fps, ok) in [(Some(0u16), false), (Some(1), true), (Some(120), true), (Some(121), false), (None, true)] {
            let node = leaf("board", NodeBody::Canvas { mouse_events: vec![], height: None, fill: None, fps });
            assert_eq!(node.validate().is_ok(), ok, "fps = {fps:?}");
        }
    }
}
