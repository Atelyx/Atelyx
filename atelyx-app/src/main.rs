//! Atelyx 进程入口：启动逻辑在 `atelyx-app` 库里，本文件只承担平台入口属性。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> iced::Result {
    atelyx_app::run()
}
