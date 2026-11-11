//! 布局与结构图标：面板分栏、对齐、缩放、连接、类型与运算符号。

use super::Shape;

/// 外框 + 左上分区（分区靠两条边线与外框相接）。
const PANEL_LAYOUT: &[Shape] = &[
    Shape::rect(3.0, 3.0, 18.0, 18.0, 2.0),
    Shape::line(10.5, 3.0, 10.5, 10.5),
    Shape::line(3.0, 10.5, 10.5, 10.5),
];

/// 四块不等大面板：分界线错位，不像 grid 那样等分。
const LAYOUT_DASHBOARD: &[Shape] = &[
    Shape::rect(3.0, 3.0, 18.0, 18.0, 2.0),
    Shape::line(3.0, 13.0, 21.0, 13.0),
    Shape::line(13.0, 3.0, 13.0, 13.0),
    Shape::line(10.0, 13.0, 10.0, 21.0),
];

/// 外框 + 顶栏 + 左侧栏：分界线贴到外框上，省掉内层套框。
const TEMPLATE: &[Shape] = &[
    Shape::rect(3.0, 3.0, 18.0, 18.0, 2.0),
    Shape::line(3.0, 9.0, 21.0, 9.0),
    Shape::line(9.0, 9.0, 9.0, 21.0),
];

const COLUMNS_2: &[Shape] = &[
    Shape::rect(3.0, 4.0, 18.0, 16.0, 2.0),
    Shape::line(12.0, 4.0, 12.0, 20.0),
];
const COLUMNS_3: &[Shape] = &[
    Shape::rect(3.0, 4.0, 18.0, 16.0, 2.0),
    Shape::line(9.0, 4.0, 9.0, 20.0),
    Shape::line(15.0, 4.0, 15.0, 20.0),
];

/// 上下夹两横线、中间短竖排矩形。
const SPACE_AROUND: &[Shape] = &[
    Shape::line(4.0, 4.0, 20.0, 4.0),
    Shape::line(4.0, 20.0, 20.0, 20.0),
    Shape::rect(9.5, 7.5, 5.0, 9.0, 1.0),
];

const ALIGN_LEFT: &[Shape] = &[
    Shape::line(4.0, 6.0, 20.0, 6.0),
    Shape::line(4.0, 12.0, 14.0, 12.0),
    Shape::line(4.0, 18.0, 18.0, 18.0),
];

const LIST: &[Shape] = &[
    Shape::line(9.0, 6.0, 20.0, 6.0),
    Shape::line(9.0, 12.0, 20.0, 12.0),
    Shape::line(9.0, 18.0, 20.0, 18.0),
    Shape::circle(5.0, 6.0, 1.2),
    Shape::circle(5.0, 12.0, 1.2),
    Shape::circle(5.0, 18.0, 1.2),
];

const ZOOM_IN: &[Shape] = &[
    Shape::circle(10.5, 10.5, 7.5),
    Shape::line(16.0, 16.0, 21.0, 21.0),
    Shape::line(7.0, 10.5, 14.0, 10.5),
    Shape::line(10.5, 7.0, 10.5, 14.0),
];
const ZOOM_OUT: &[Shape] = &[
    Shape::circle(10.5, 10.5, 7.5),
    Shape::line(16.0, 16.0, 21.0, 21.0),
    Shape::line(7.0, 10.5, 14.0, 10.5),
];

/// 链的两半沿对角拉开：每半 = 半圆帽 + 两条平行链边，中间留断口。
const UNLINK: &[Shape] = &[
    Shape::arc(6.84, 17.16, 2.6, 45.0, 225.0),
    Shape::line(8.68, 19.0, 10.94, 16.74),
    Shape::line(5.0, 15.32, 7.26, 13.06),
    Shape::arc(17.16, 6.84, 2.6, 225.0, 405.0),
    Shape::line(15.32, 5.0, 13.06, 7.26),
    Shape::line(19.0, 8.68, 16.74, 10.94),
];

/// 两个共用对角线的链环互相咬合。
const LINK: &[Shape] = &[
    Shape::circle(9.6, 14.4, 3.7),
    Shape::circle(14.4, 9.6, 3.7),
];

const LAYERS: &[Shape] = &[
    Shape::area(&[[12.0, 3.5], [20.0, 8.0], [12.0, 12.5], [4.0, 8.0]]),
    Shape::poly(&[[4.0, 12.0], [12.0, 16.5], [20.0, 12.0]]),
    Shape::poly(&[[4.0, 16.0], [12.0, 20.5], [20.0, 16.0]]),
];

/// 等轴测方块：外轮廓一次画出，三条内棱线汇于顶面中心。
const BOX: &[Shape] = &[
    Shape::area(&[[12.0, 3.0], [20.0, 7.5], [20.0, 16.5], [12.0, 21.0], [4.0, 16.5], [4.0, 7.5]]),
    Shape::line(4.0, 7.5, 12.0, 12.0),
    Shape::line(20.0, 7.5, 12.0, 12.0),
    Shape::line(12.0, 12.0, 12.0, 21.0),
];

const SQUARE: &[Shape] = &[Shape::rect(4.0, 4.0, 16.0, 16.0, 1.5)];

const TOGGLE_LEFT: &[Shape] = &[
    Shape::rect(2.5, 6.0, 19.0, 12.0, 6.0),
    Shape::circle(8.0, 12.0, 2.6),
];

const TYPE: &[Shape] = &[
    Shape::line(5.0, 6.5, 19.0, 6.5),
    Shape::line(12.0, 6.5, 12.0, 19.5),
    Shape::line(8.5, 19.5, 15.5, 19.5),
];

/// 五边形标签：方形角在左上，尖端朝右下，孔靠近挂孔角。
const TAG: &[Shape] = &[
    Shape::area(&[[3.5, 3.5], [13.0, 3.5], [20.5, 11.0], [11.0, 20.5], [3.5, 13.0]]),
    Shape::circle(7.5, 7.5, 1.4),
];

const HASH: &[Shape] = &[
    Shape::line(9.5, 3.5, 7.5, 20.5),
    Shape::line(16.5, 3.5, 14.5, 20.5),
    Shape::line(3.5, 8.5, 20.5, 8.5),
    Shape::line(3.5, 15.5, 20.5, 15.5),
];

const SIGMA: &[Shape] = &[Shape::poly(&[[18.5, 4.5], [5.0, 4.5], [13.0, 12.0], [5.0, 19.5], [18.5, 19.5]])];

const CODE: &[Shape] = &[
    Shape::poly(&[[8.5, 6.0], [3.0, 12.0], [8.5, 18.0]]),
    Shape::poly(&[[15.5, 6.0], [21.0, 12.0], [15.5, 18.0]]),
];

/// 本组图标名与形状（kebab 名按此顺序导出）。
pub(super) const ICONS: &[(&str, &[Shape])] = &[
    ("panel-layout", PANEL_LAYOUT),
    ("layout-dashboard", LAYOUT_DASHBOARD),
    ("template", TEMPLATE),
    ("columns-2", COLUMNS_2),
    ("columns-3", COLUMNS_3),
    ("space-around", SPACE_AROUND),
    ("align-left", ALIGN_LEFT),
    ("list", LIST),
    ("zoom-in", ZOOM_IN),
    ("zoom-out", ZOOM_OUT),
    ("unlink", UNLINK),
    ("link", LINK),
    ("layers", LAYERS),
    ("box", BOX),
    ("square", SQUARE),
    ("toggle-left", TOGGLE_LEFT),
    ("type", TYPE),
    ("tag", TAG),
    ("hash", HASH),
    ("sigma", SIGMA),
    ("code", CODE),
];
