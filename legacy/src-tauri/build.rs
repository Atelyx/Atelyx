fn main() {
    // 安卓产物必须按 16KB 页对齐：构建时注入的 RUSTFLAGS 会整体覆盖 .cargo/config.toml 的
    // rustflags，64 位 ABI 还能靠链接器默认值蒙混过关，32 位 ABI 会退回 4KB，装到 Android 15+
    // 设备上加载失败。这里走 link-arg 追加，绕开 RUSTFLAGS 的覆盖。
    if std::env::var("TARGET").map_or(false, |t| t.contains("android")) {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
    }
    tauri_build::build()
}
