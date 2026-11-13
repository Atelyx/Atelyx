//! 方向与导航图标：箭头、折角、菜单点列、视图手势。

use super::Shape;

/// 三点横排（菜单「更多」与省略号同形）。
const THREE_DOTS: &[Shape] = &[
    Shape::circle(5.0, 12.0, 1.0),
    Shape::circle(12.0, 12.0, 1.0),
    Shape::circle(19.0, 12.0, 1.0),
];

const ARROW_LEFT: &[Shape] = &[
    Shape::line(19.0, 12.0, 5.0, 12.0),
    Shape::poly(&[[12.0, 19.0], [5.0, 12.0], [12.0, 5.0]]),
];
const ARROW_RIGHT: &[Shape] = &[
    Shape::line(5.0, 12.0, 19.0, 12.0),
    Shape::poly(&[[12.0, 5.0], [19.0, 12.0], [12.0, 19.0]]),
];
const ARROW_UP: &[Shape] = &[
    Shape::line(12.0, 19.0, 12.0, 5.0),
    Shape::poly(&[[5.0, 12.0], [12.0, 5.0], [19.0, 12.0]]),
];
const ARROW_DOWN: &[Shape] = &[
    Shape::line(12.0, 5.0, 12.0, 19.0),
    Shape::poly(&[[5.0, 12.0], [12.0, 19.0], [19.0, 12.0]]),
];
const ARROW_UP_DOWN: &[Shape] = &[
    Shape::line(17.0, 4.0, 17.0, 20.0),
    Shape::poly(&[[13.0, 16.0], [17.0, 20.0], [21.0, 16.0]]),
    Shape::line(7.0, 20.0, 7.0, 4.0),
    Shape::poly(&[[3.0, 8.0], [7.0, 4.0], [11.0, 8.0]]),
];
const CHEVRON_DOWN: &[Shape] = &[Shape::poly(&[[6.0, 9.5], [12.0, 15.5], [18.0, 9.5]])];
const CHEVRON_UP: &[Shape] = &[Shape::poly(&[[6.0, 14.5], [12.0, 8.5], [18.0, 14.5]])];
const CHEVRON_RIGHT: &[Shape] = &[Shape::poly(&[[9.0, 6.0], [15.0, 12.0], [9.0, 18.0]])];
const CHEVRON_LEFT: &[Shape] = &[Shape::poly(&[[15.0, 6.0], [9.0, 12.0], [15.0, 18.0]])];
const CHEVRONS_UP_DOWN: &[Shape] = &[
    Shape::poly(&[[7.0, 15.0], [12.0, 20.0], [17.0, 15.0]]),
    Shape::poly(&[[7.0, 9.0], [12.0, 4.0], [17.0, 9.0]]),
];
const CHEVRONS_DOWN_UP: &[Shape] = &[
    Shape::poly(&[[7.0, 20.0], [12.0, 15.0], [17.0, 20.0]]),
    Shape::poly(&[[7.0, 4.0], [12.0, 9.0], [17.0, 4.0]]),
];
const CORNER_UP_LEFT: &[Shape] = &[
    Shape::poly(&[[20.0, 20.0], [20.0, 13.0], [19.0, 11.0], [16.5, 9.6], [4.0, 9.0]]),
    Shape::poly(&[[9.0, 14.0], [4.0, 9.0], [9.0, 4.0]]),
];
const MENU: &[Shape] = &[
    Shape::line(4.0, 5.5, 20.0, 5.5),
    Shape::line(4.0, 12.0, 20.0, 12.0),
    Shape::line(4.0, 18.5, 20.0, 18.5),
];
const GRIP_VERTICAL: &[Shape] = &[
    Shape::circle(9.0, 5.0, 1.0),
    Shape::circle(9.0, 12.0, 1.0),
    Shape::circle(9.0, 19.0, 1.0),
    Shape::circle(15.0, 5.0, 1.0),
    Shape::circle(15.0, 12.0, 1.0),
    Shape::circle(15.0, 19.0, 1.0),
];
/// 两列三点横排（表格字段拖拽把手）。
const GRIP_HORIZONTAL: &[Shape] = &[
    Shape::circle(9.0, 9.0, 1.0),
    Shape::circle(15.0, 9.0, 1.0),
    Shape::circle(9.0, 12.0, 1.0),
    Shape::circle(15.0, 12.0, 1.0),
    Shape::circle(9.0, 15.0, 1.0),
    Shape::circle(15.0, 15.0, 1.0),
];
const MAXIMIZE: &[Shape] = &[
    Shape::poly(&[[9.0, 3.0], [5.0, 3.0], [3.0, 5.0], [3.0, 9.0]]),
    Shape::poly(&[[15.0, 3.0], [19.0, 3.0], [21.0, 5.0], [21.0, 9.0]]),
    Shape::poly(&[[3.0, 15.0], [3.0, 19.0], [5.0, 21.0], [9.0, 21.0]]),
    Shape::poly(&[[21.0, 15.0], [21.0, 19.0], [19.0, 21.0], [15.0, 21.0]]),
];
const MINIMIZE: &[Shape] = &[
    Shape::poly(&[[9.0, 3.0], [9.0, 5.0], [5.0, 5.0], [5.0, 9.0]]),
    Shape::poly(&[[15.0, 3.0], [15.0, 5.0], [19.0, 5.0], [19.0, 9.0]]),
    Shape::poly(&[[5.0, 15.0], [5.0, 19.0], [9.0, 19.0], [9.0, 21.0]]),
    Shape::poly(&[[19.0, 15.0], [19.0, 19.0], [15.0, 19.0], [15.0, 21.0]]),
];
const EXPAND: &[Shape] = &[
    Shape::poly(&[[14.0, 3.0], [21.0, 3.0], [21.0, 10.0]]),
    Shape::poly(&[[10.0, 21.0], [3.0, 21.0], [3.0, 14.0]]),
    Shape::line(21.0, 3.0, 14.0, 10.0),
    Shape::line(3.0, 21.0, 10.0, 14.0),
];
const LOG_IN: &[Shape] = &[
    Shape::poly(&[[10.0, 17.0], [15.0, 12.0], [10.0, 7.0]]),
    Shape::line(15.0, 12.0, 3.0, 12.0),
    Shape::poly(&[[15.0, 3.0], [19.0, 3.0], [21.0, 5.0], [21.0, 19.0], [19.0, 21.0], [15.0, 21.0]]),
];
const EXTERNAL_LINK: &[Shape] = &[
    Shape::poly(&[[13.0, 4.0], [20.0, 4.0], [20.0, 11.0]]),
    Shape::line(20.0, 4.0, 11.5, 12.5),
    Shape::poly(&[[17.0, 14.5], [17.0, 19.0], [15.0, 21.0], [5.0, 21.0], [3.0, 19.0], [3.0, 9.0], [5.0, 7.0], [9.5, 7.0]]),
];
const SEARCH: &[Shape] = &[
    Shape::circle(10.8, 10.8, 7.8),
    Shape::line(16.6, 16.6, 21.0, 21.0),
];

/// 本组图标名与形状（kebab 名按此顺序导出）。
pub(super) const ICONS: &[(&str, &[Shape])] = &[
    ("arrow-left", ARROW_LEFT),
    ("arrow-right", ARROW_RIGHT),
    ("arrow-up", ARROW_UP),
    ("arrow-down", ARROW_DOWN),
    ("arrow-up-down", ARROW_UP_DOWN),
    ("chevron-down", CHEVRON_DOWN),
    ("chevron-up", CHEVRON_UP),
    ("chevron-right", CHEVRON_RIGHT),
    ("chevron-left", CHEVRON_LEFT),
    ("chevrons-up-down", CHEVRONS_UP_DOWN),
    ("chevrons-down-up", CHEVRONS_DOWN_UP),
    ("corner-up-left", CORNER_UP_LEFT),
    ("menu", MENU),
    ("more-horizontal", THREE_DOTS),
    ("ellipsis", THREE_DOTS),
    ("grip-vertical", GRIP_VERTICAL),
    ("grip-horizontal", GRIP_HORIZONTAL),
    ("maximize", MAXIMIZE),
    ("minimize", MINIMIZE),
    ("expand", EXPAND),
    ("log-in", LOG_IN),
    ("search", SEARCH),
    ("external-link", EXTERNAL_LINK),
];