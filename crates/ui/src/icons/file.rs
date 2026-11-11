//! 文件与仓库图标：目录、文档、历史、网络。

use super::Shape;

const FOLDER: &[Shape] = &[
    Shape::area(&[[3.0, 20.0], [3.0, 5.0], [8.0, 5.0], [10.5, 8.0], [21.0, 8.0], [21.0, 20.0]]),
];
const FOLDER_OPEN: &[Shape] = &[
    // 后壁不封底：底边由前片补齐
    Shape::poly(&[[3.0, 18.0], [3.0, 5.0], [8.0, 5.0], [10.5, 8.0], [21.0, 8.0], [21.0, 11.0]]),
    Shape::area(&[[3.0, 19.0], [7.0, 11.0], [21.0, 11.0], [18.0, 19.0]]),
];
const FOLDER_PLUS: &[Shape] = &[
    Shape::area(&[[3.0, 20.0], [3.0, 5.0], [8.0, 5.0], [10.5, 8.0], [21.0, 8.0], [21.0, 20.0]]),
    Shape::line(12.0, 11.5, 12.0, 16.5),
    Shape::line(9.5, 14.0, 14.5, 14.0),
];
const FOLDER_TREE: &[Shape] = &[
    Shape::rect(12.0, 3.0, 9.0, 6.0, 1.5),
    Shape::rect(12.0, 15.0, 9.0, 6.0, 1.5),
    // 一条折线走完上枝、主干、下枝
    Shape::poly(&[[12.0, 6.0], [3.0, 6.0], [3.0, 18.0], [12.0, 18.0]]),
];
const FILE_TEXT: &[Shape] = &[
    Shape::area(&[[5.0, 21.0], [5.0, 3.0], [14.0, 3.0], [19.0, 8.0], [19.0, 21.0]]),
    Shape::poly(&[[14.0, 3.0], [14.0, 8.0], [19.0, 8.0]]),
    Shape::line(9.0, 12.0, 15.0, 12.0),
    Shape::line(9.0, 16.0, 15.0, 16.0),
];
const FILE_PLUS: &[Shape] = &[
    Shape::area(&[[5.0, 21.0], [5.0, 3.0], [14.0, 3.0], [19.0, 8.0], [19.0, 21.0]]),
    Shape::poly(&[[14.0, 3.0], [14.0, 8.0], [19.0, 8.0]]),
    Shape::line(11.5, 12.0, 11.5, 18.0),
    Shape::line(8.5, 15.0, 14.5, 15.0),
];
const FILE_OUTPUT: &[Shape] = &[
    Shape::area(&[[4.0, 20.0], [4.0, 4.0], [9.0, 4.0], [13.0, 8.0], [13.0, 20.0]]),
    Shape::poly(&[[9.0, 4.0], [9.0, 8.0], [13.0, 8.0]]),
    Shape::line(10.5, 14.0, 21.0, 14.0),
    Shape::poly(&[[17.5, 10.5], [21.0, 14.0], [17.5, 17.5]]),
];
const FILES: &[Shape] = &[
    Shape::poly(&[[8.0, 3.0], [18.0, 3.0], [18.0, 14.0]]),
    Shape::area(&[[4.0, 7.0], [11.0, 7.0], [15.0, 11.0], [15.0, 21.0], [4.0, 21.0]]),
    Shape::poly(&[[11.0, 7.0], [11.0, 11.0], [15.0, 11.0]]),
];
const STICKY_NOTE: &[Shape] = &[
    Shape::area(&[[4.0, 4.0], [20.0, 4.0], [20.0, 15.0], [15.0, 20.0], [4.0, 20.0]]),
    Shape::poly(&[[15.0, 20.0], [15.0, 15.0], [20.0, 15.0]]),
];
const PAPERCLIP: &[Shape] = &[
    // 斜置一笔：外圈长边—大 U 回折—回折边—小 U 回钩—内尾；两个 U 弧各扫半圈
    Shape::line(3.5, 13.8, 12.8, 6.0),
    Shape::arc(15.8, 9.5, 4.7, -130.0, 50.0),
    Shape::line(18.8, 13.1, 12.3, 18.6),
    Shape::arc(10.8, 16.8, 2.3, 50.0, 230.0),
    Shape::line(9.3, 15.0, 14.4, 10.7),
];
const TABLE: &[Shape] = &[
    // 内部分隔线两端内缩一格：贴到框线上会在小尺寸糊成加粗边
    Shape::rect(3.0, 5.0, 18.0, 14.0, 2.0),
    Shape::line(4.0, 9.5, 20.0, 9.5),
    Shape::line(9.5, 9.5, 9.5, 18.0),
];
const HISTORY: &[Shape] = &[
    // 外圈在左上方留缺口；箭头两支从弧端沿切线张开，指向逆时针回扫方向
    Shape::arc(12.0, 12.5, 8.3, 255.0, 555.0),
    Shape::poly(&[[6.8, 3.7], [9.9, 4.5], [7.6, 6.8]]),
    Shape::poly(&[[12.0, 8.6], [12.0, 12.5], [15.2, 14.2]]),
];
const CLOCK: &[Shape] = &[
    Shape::circle(12.0, 12.0, 8.5),
    Shape::poly(&[[12.0, 7.5], [12.0, 12.0], [15.5, 13.5]]),
];
const CALENDAR_DAYS: &[Shape] = &[
    // 横线两端内缩一格：端点圆头压在框线上会长出小凸起
    Shape::rect(3.0, 5.0, 18.0, 16.0, 2.0),
    Shape::line(4.0, 9.5, 20.0, 9.5),
    Shape::line(8.0, 3.0, 8.0, 6.5),
    Shape::line(16.0, 3.0, 16.0, 6.5),
    Shape::circle(9.0, 14.5, 1.0),
    Shape::circle(15.0, 14.5, 1.0),
];
const BOOKMARK_PLUS: &[Shape] = &[
    Shape::area(&[[6.0, 3.0], [18.0, 3.0], [18.0, 21.0], [12.0, 16.5], [6.0, 21.0]]),
    Shape::line(12.0, 7.5, 12.0, 13.0),
    Shape::line(9.0, 10.25, 15.0, 10.25),
];
const BOOKMARK_MINUS: &[Shape] = &[
    Shape::area(&[[6.0, 3.0], [18.0, 3.0], [18.0, 21.0], [12.0, 16.5], [6.0, 21.0]]),
    Shape::line(9.0, 10.25, 15.0, 10.25),
];
const IMAGE: &[Shape] = &[
    // 山脊两点略离框线，圆头端点贴在框上会晕成粗角
    Shape::rect(3.0, 3.0, 18.0, 18.0, 2.0),
    Shape::poly(&[[4.2, 20.2], [9.5, 12.5], [14.8, 20.2]]),
    Shape::circle(8.5, 8.0, 1.5),
];
const IMAGE_PLUS: &[Shape] = &[
    // 图框让出右上角给加号：框缩小一档，14px 下加号才不会和框线糊成一片
    Shape::rect(3.0, 7.0, 14.0, 14.0, 2.0),
    Shape::poly(&[[3.4, 20.4], [9.0, 14.0], [14.6, 20.4]]),
    Shape::circle(7.5, 11.0, 1.3),
    Shape::line(19.0, 2.6, 19.0, 8.2),
    Shape::line(16.2, 5.4, 21.8, 5.4),
];
const DOWNLOAD: &[Shape] = &[
    Shape::line(12.0, 3.0, 12.0, 14.5),
    Shape::poly(&[[6.5, 9.0], [12.0, 14.5], [17.5, 9.0]]),
    Shape::poly(&[[4.0, 15.0], [4.0, 19.0], [6.0, 21.0], [18.0, 21.0], [20.0, 19.0], [20.0, 15.0]]),
];
const HARD_DRIVE: &[Shape] = &[
    // 分隔线两端内缩一格：圆头端点压在框线上会长出小凸起
    Shape::rect(3.0, 6.0, 18.0, 12.0, 2.0),
    Shape::line(5.0, 13.0, 19.0, 13.0),
    Shape::circle(7.0, 16.5, 1.0),
    Shape::circle(11.5, 16.5, 1.0),
];
const SERVER: &[Shape] = &[
    Shape::rect(3.0, 4.0, 18.0, 7.0, 2.0),
    Shape::rect(3.0, 13.0, 18.0, 7.0, 2.0),
    Shape::circle(7.0, 7.5, 1.0),
    Shape::circle(7.0, 16.5, 1.0),
];
const CLOUD: &[Shape] = &[
    // 左大弧与右肩弧各自接到基线两端、由短线相接：四段首尾闭合，接缝不出尖角
    Shape::line(16.7, 18.0, 9.4, 18.0),
    Shape::arc(9.4, 12.0, 6.0, 90.0, 343.4),
    Shape::line(15.2, 10.3, 16.7, 10.3),
    Shape::arc(16.7, 14.1, 3.9, 270.0, 450.0),
];
const GLOBE: &[Shape] = &[
    Shape::circle(12.0, 12.0, 9.0),
    Shape::line(3.0, 12.0, 21.0, 12.0),
    // 经线是椭圆，用两段大半径圆弧逼近；弧端正好落在外圈上下极点上
    Shape::arc(17.6, 12.0, 10.6, 121.9, 238.1),
    Shape::arc(6.4, 12.0, 10.6, -58.1, 58.1),
];
const TICKET: &[Shape] = &[
    // 撕线用两段短线（一段太长在 14px 会读成"分栏"）
    Shape::rect(3.0, 6.0, 18.0, 12.0, 2.0),
    Shape::line(14.0, 7.0, 14.0, 10.0),
    Shape::line(14.0, 12.0, 14.0, 17.0),
];

/// 本组图标名与形状（kebab 名按此顺序导出）。
pub(super) const ICONS: &[(&str, &[Shape])] = &[
    ("folder", FOLDER),
    ("folder-open", FOLDER_OPEN),
    ("folder-plus", FOLDER_PLUS),
    ("folder-tree", FOLDER_TREE),
    ("file-text", FILE_TEXT),
    ("file-plus", FILE_PLUS),
    ("file-output", FILE_OUTPUT),
    ("files", FILES),
    ("sticky-note", STICKY_NOTE),
    ("paperclip", PAPERCLIP),
    ("table", TABLE),
    ("history", HISTORY),
    ("clock", CLOCK),
    ("calendar-days", CALENDAR_DAYS),
    ("bookmark-plus", BOOKMARK_PLUS),
    ("bookmark-minus", BOOKMARK_MINUS),
    ("image", IMAGE),
    ("image-plus", IMAGE_PLUS),
    ("download", DOWNLOAD),
    ("hard-drive", HARD_DRIVE),
    ("server", SERVER),
    ("cloud", CLOUD),
    ("globe", GLOBE),
    ("ticket", TICKET),
];
