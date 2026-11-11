//! 编辑与操作图标：增删、复制、书写、清理、撤销与播放控制。

use super::Shape;

// 齿轮：圆身 + 四个方齿；齿脚插进圆周内侧，交界处不留断口。
const SETTINGS: &[Shape] = &[
    Shape::circle(12.0, 12.0, 6.9),
    Shape::poly(&[[9.7, 6.4], [9.7, 2.6], [14.3, 2.6], [14.3, 6.4]]),
    Shape::poly(&[[17.6, 9.7], [21.4, 9.7], [21.4, 14.3], [17.6, 14.3]]),
    Shape::poly(&[[14.3, 17.6], [14.3, 21.4], [9.7, 21.4], [9.7, 17.6]]),
    Shape::poly(&[[6.4, 14.3], [2.6, 14.3], [2.6, 9.7], [6.4, 9.7]]),
];

const PLUS: &[Shape] = &[
    Shape::line(12.0, 5.0, 12.0, 19.0),
    Shape::line(5.0, 12.0, 19.0, 12.0),
];
const MINUS: &[Shape] = &[Shape::line(5.0, 12.0, 19.0, 12.0)];
const X: &[Shape] = &[
    Shape::line(6.0, 6.0, 18.0, 18.0),
    Shape::line(18.0, 6.0, 6.0, 18.0),
];
const CHECK: &[Shape] = &[Shape::poly(&[[5.0, 13.0], [10.0, 18.0], [19.0, 6.5]])];

// 两页纸：后页只留左上两条可见边，前页整框压住其余部分。
const COPY: &[Shape] = &[
    Shape::poly(&[[15.0, 4.5], [6.0, 4.5], [4.5, 6.0], [4.5, 15.0]]),
    Shape::rect(8.5, 8.5, 11.5, 11.5, 2.0),
];

// 铅笔走 45° 轴：笔身是绕轴摆正的矩形，笔尖三角与笔身分线共点。
const PENCIL: &[Shape] = &[
    Shape::area(&[[4.5, 19.5], [8.2, 18.9], [15.2, 11.9], [12.1, 8.8], [5.1, 15.8]]),
    Shape::line(5.1, 15.8, 8.2, 18.9),
];

const PEN_LINE: &[Shape] = &[
    Shape::area(&[[3.5, 17.5], [6.7, 17.0], [12.7, 11.0], [10.0, 8.3], [4.0, 14.3]]),
    Shape::line(4.0, 14.3, 6.7, 17.0),
    Shape::line(12.5, 20.0, 21.0, 20.0),
];

const BIN: &[Shape] = &[
    Shape::line(4.0, 5.5, 20.0, 5.5),
    Shape::poly(&[[9.0, 5.5], [9.0, 3.0], [15.0, 3.0], [15.0, 5.5]]),
    Shape::poly(&[[6.2, 5.5], [6.2, 20.0], [17.8, 20.0], [17.8, 5.5]]),
    Shape::line(10.0, 9.5, 10.0, 16.5),
    Shape::line(14.0, 9.5, 14.0, 16.5),
];

// 橡皮斜放 45°，分界线平行于短边，底边压一条基准线。
const ERASER: &[Shape] = &[
    Shape::area(&[[8.0, 20.0], [3.5, 15.5], [13.0, 6.0], [17.5, 10.5]]),
    Shape::line(9.2, 9.8, 13.7, 14.3),
    Shape::line(3.0, 20.0, 21.0, 20.0),
];

const SCISSORS: &[Shape] = &[
    Shape::circle(6.0, 6.5, 2.5),
    Shape::circle(6.0, 17.5, 2.5),
    Shape::line(8.4, 8.0, 19.5, 16.0),
    Shape::line(8.4, 16.0, 19.5, 8.0),
];

const CLIPBOARD_PASTE: &[Shape] = &[
    Shape::rect(5.0, 5.0, 14.0, 16.0, 2.0),
    Shape::rect(9.0, 3.0, 6.0, 4.0, 1.5),
    Shape::rect(9.2, 11.2, 7.6, 7.2, 1.0),
];

// 标题横线左端接逆时针回扫箭头，尾部绕右侧回到下方。
const UNDO: &[Shape] = &[
    Shape::poly(&[[9.0, 4.5], [4.0, 9.0], [9.0, 13.5]]),
    Shape::line(4.0, 9.0, 14.0, 9.0),
    Shape::arc(14.0, 14.5, 5.5, 270.0, 90.0),
    Shape::line(14.0, 20.0, 10.0, 20.0),
];

// 缺口开在右侧；箭头顶点落在弧端，两臂朝行进反方向张成 V。
const REFRESH: &[Shape] = &[
    Shape::arc(12.0, 12.0, 6.5, 60.0, 330.0),
    Shape::poly(&[[18.3, 4.8], [17.6, 8.8], [13.5, 8.0]]),
];

// 缺约四分之一圈的圆弧（转圈动画按它旋转）。
const LOADER: &[Shape] = &[Shape::arc(12.0, 12.0, 7.0, 30.0, 300.0)];

const PLAY: &[Shape] = &[Shape::area(&[[8.5, 5.0], [19.0, 12.0], [8.5, 19.0]])];
const PAUSE: &[Shape] = &[
    Shape::line(9.0, 5.0, 9.0, 19.0),
    Shape::line(15.0, 5.0, 15.0, 19.0),
];
const STOP: &[Shape] = &[Shape::rect(6.0, 6.0, 12.0, 12.0, 2.5)];

// 调色盘：右侧拇指缺口，盘内三团颜料靠里放（贴边会与盘沿糊在一起）。
const PALETTE: &[Shape] = &[
    Shape::arc(12.0, 12.0, 7.0, 22.0, 338.0),
    Shape::poly(&[[18.5, 14.6], [15.6, 13.9], [15.6, 10.1], [18.5, 9.4]]),
    Shape::circle(8.8, 8.8, 1.2),
    Shape::circle(7.4, 12.0, 1.2),
    Shape::circle(8.8, 15.2, 1.2),
];

const KEYBOARD: &[Shape] = &[
    Shape::rect(3.0, 6.5, 18.0, 11.0, 2.0),
    Shape::line(6.5, 10.0, 8.5, 10.0),
    Shape::line(11.0, 10.0, 13.0, 10.0),
    Shape::line(15.5, 10.0, 17.5, 10.0),
    Shape::line(8.0, 14.0, 16.0, 14.0),
];

/// 本组图标名与形状（kebab 名按此顺序导出）。
pub(super) const ICONS: &[(&str, &[Shape])] = &[
    ("settings", SETTINGS),
    ("plus", PLUS),
    ("minus", MINUS),
    ("x", X),
    ("check", CHECK),
    ("copy", COPY),
    ("pencil", PENCIL),
    ("pen-line", PEN_LINE),
    ("bin", BIN),
    ("eraser", ERASER),
    ("scissors", SCISSORS),
    ("clipboard-paste", CLIPBOARD_PASTE),
    ("undo", UNDO),
    ("refresh", REFRESH),
    ("loader", LOADER),
    ("play", PLAY),
    ("pause", PAUSE),
    ("stop", STOP),
    ("palette", PALETTE),
    ("keyboard", KEYBOARD),
];
