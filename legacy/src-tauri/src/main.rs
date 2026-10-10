// 防止 Windows 发生控制台窗口闪烁
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    atelyx_lib::run()
}
