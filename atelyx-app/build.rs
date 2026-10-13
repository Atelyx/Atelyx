//! Windows 构建期把应用图标编进 exe 资源，供资源管理器 / 开始菜单 / 快捷方式显示。
fn main() {
    // build.rs 编译运行在宿主机上，目标平台只能在运行时按 CARGO_CFG_TARGET_OS 判定
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 声明任一路径后 cargo 不再按包内任意文件变更重跑本脚本，版本号与图标都要列全
        println!("cargo:rerun-if-changed=Cargo.toml");
        println!("cargo:rerun-if-changed=assets/icons/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/icons/icon.ico")
            .compile()
            .expect("exe 图标资源编译失败（需要 Windows SDK 的 rc.exe）");
    }
}
