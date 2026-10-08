//! Atelyx Tauri 后端入口：命令注册（generate_handler）与各托管状态；
//! 文件化仓库（`vault.rs`）为唯一存储出口。

// Windows 上 MSVC 链接 dll 时会顺带产出导入库并向 stdout 打印「正在创建库 …dll.lib 和对象
// …dll.exp」（/NOLOGO 也不抑制），rustc 把这段输出当 linker_messages 警告上报；属预期行为，
// 关掉该 lint。
#![allow(linker_messages)]

#[cfg(target_os = "android")]
mod android_bridge;
mod chat_container;
mod commands;
mod layout;
mod layout_drag;
mod layout_model;
mod layout_persist;
mod layout_window;
mod net_guard;
mod plugin_build;
mod plugin_process;
mod tray;
mod vault;

use std::sync::Arc;

use tauri::Manager;

/// 应用入口：桌面由 lib/main 调用；移动端由 tauri mobile 生成的原生工程经 JNI 调起
/// （mobile_entry_point 导出宿主入口）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // 单实例（桌面，须最先注册）：第二个进程启动即退出，并在首个实例内回调。
    // 登录自启的进程带 --autorun → 静默退出不抢焦点；用户手动二次启动 → 显示全部窗口
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if args.iter().any(|arg| arg == "--autorun") {
            return;
        }
        tray::show_all_windows(app);
    }));
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init());
    // 桌面壳专属插件：shell（系统打开）/ autostart（开机自启，注册项附带 --autorun 供上面的
    // 单实例回调识别登录自启）/ global-shortcut（OS 级全局快捷键，ctx.shortcuts 后端，
    // 触发事件统一转投主窗口，见 commands/global_shortcut.rs）；移动端无对应实现，不注册
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autorun"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build());
    let app = builder
        .setup(|app| {
            // 仓库化：注册当前仓库根路径状态（初始为 None，open_vault 时设置）
            app.manage(vault::VaultState::default());
            // 应用内更新下载：托管在途任务的取消句柄（见 commands/update.rs）
            app.manage(commands::update::UpdateDownloadState::default());
            // 布局迷你窗口管理器：布局模型唯一权威，启动即从 ui-state.json 加载
            app.manage(layout::LayoutState::new());
            layout::load_from_disk(app.handle(), &app.state::<layout::LayoutState>());
            // 插件托管进程：进程创建即纳入作业对象/进程组，随应用退出统一收尾（见 plugin_process.rs）
            app.manage(Arc::new(plugin_process::PluginProcessHost::new()));
            // 打开文件上下文宿主（跨窗口协调态真源：主窗口唯一写者，撕裂窗口拉基线 + 订阅广播）
            app.manage(commands::open_context::OpenContextState::default());
            // 全局快捷键登记表（ctx.shortcuts 后端；移动端空表，注册恒拒、注销/释放幂等成功）
            app.manage(commands::global_shortcut::GlobalShortcutState::default());
            // 会话容器真源：唯一 op 权威与写盘链；写链线程随进程常驻。
            // 测试构建不接线——emit 实例化只允许进生产链接闭包（install_app_emitter 仅生产编译）。
            #[cfg(not(test))]
            {
                let chat_state = Arc::new(chat_container::ChatContainerState::new(
                    chat_container::WriteChainConfig::default(),
                ));
                chat_state.install_app_emitter(app.handle().clone());
                chat_state.spawn_writer();
                app.manage(chat_state);
            }
            // 主窗口窗口事件钩子：Moved/Resized → 权威 bounds（拖拽命中/落点解析）。
            // 桌面专属：移动端单窗口无移动/缩放语义
            #[cfg(desktop)]
            if let Some(main_win) = app.get_webview_window("main") {
                main_win.on_window_event(layout::window_event_handler(app.handle(), "main".into()));
                // 种子化初始 bounds：启动后未移动过时 on_window_event 不触发，拖拽解析读不到
                layout::seed_window_bounds(app.handle(), "main");
            }
            // 托盘与驻留标志（桌面）：登录自启（--autorun）静默启动，UI 藏在托盘不显示
            // 主窗口；手动启动照常显示。主窗口 visible 先由配置置 false，此处按启动方式
            // 决定是否 show。驻留标志供撕裂窗口建窗决定可见性（见 tray.rs）。
            #[cfg(desktop)]
            {
                let autorun = std::env::args().any(|arg| arg == "--autorun");
                app.manage(tray::UiHidden::new(autorun));
                app.manage(tray::ExitWait::default());
                app.manage(tray::PluginMenus::default());
                if !autorun {
                    if let Some(main_win) = app.get_webview_window("main") {
                        // 主题底色：配置里只有深色缺省，浅色主题先改底色再显窗，不闪深色
                        let _ = main_win
                            .set_background_color(Some(commands::windows::startup_background(app.handle())));
                        let _ = main_win.show();
                    }
                }
                tray::create_tray(app.handle())?;
            }
            // 插件目录：先对账恢复更新中途崩溃被搬走的插件目录（.bak-* 即时恢复），再清扫超龄残留
            let sweep_app = app.handle().clone();
            std::thread::spawn(move || {
                if let Ok(dir) = sweep_app.path().app_data_dir() {
                    commands::plugin::reconcile_plugin_backups(&sweep_app, &dir.join("plugins"));
                    commands::plugin::sweep_plugin_residues(&dir.join("plugins"));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 仓库文件化命令
            commands::vault::open_vault,
            commands::vault::list_canvases_vault,
            commands::vault::read_canvas_vault,
            commands::vault::write_canvas_vault,
            commands::vault::patch_canvas_vault,
            commands::vault::rename_canvas_vault,
            commands::vault::move_canvas_vault,
            commands::vault::delete_canvas_vault,
            commands::vault::read_note,
            commands::vault::file_exists,
            commands::vault::scan_wiki_backlinks,
            commands::vault::scan_vault_tags,
            commands::vault::rebuild_internal_links,
            commands::vault::write_note,
            commands::vault::read_vault_file,
            commands::vault::read_vault_file_window,
            commands::vault::write_vault_file,
            commands::vault::list_vault_dir,
            commands::vault::rename_note,
            commands::vault::read_vault_config,
            commands::vault::vault_config_patch,
            // 未入库附件与表格图片（粘贴/拖入/导入先落仓库内隐藏临时区，实体只存路径引用）
            commands::temp_attachment::write_temp_attachment,
            commands::temp_attachment::import_vault_attachment,
            commands::temp_attachment::cleanup_temp_attachments,
            commands::vault::read_prompt_notes,
            commands::vault::write_prompt_notes,
            commands::vault::read_agents,
            commands::vault::write_agents,
            commands::vault::read_folder_colors,
            commands::vault::write_folder_colors,
            commands::vault::list_chat_sessions,
            commands::vault::read_chat_session_meta,
            commands::vault::write_chat_session_meta,
            commands::vault::delete_chat_session_meta,
            commands::vault::read_editor_chats_meta,
            commands::vault::write_editor_chats_meta,
            commands::vault::read_chat_messages,
            commands::vault::write_chat_messages,
            commands::vault::append_chat_messages,
            commands::vault::delete_chat_messages,
            // 会话容器真源（撕裂窗口拉基线/提交写意图；执行体提交编排变更）
            commands::chat_container::chat_container_snapshot,
            commands::chat_container::chat_container_load,
            commands::chat_container::chat_container_apply,
            commands::chat_container::chat_container_commit,
            commands::chat_container::chat_container_intent_result,
            commands::chat_container::chat_container_executor_boot,
            commands::chat_container::chat_container_flush,
            commands::vault::create_canvas_vault,
            // 仓库文件管理（全仓库文件树 + 建文件夹 + 删改 + 附件 dataURL + 链接维护）
            commands::vault::list_vault_tree,
            commands::vault::create_folder,
            commands::vault::delete_folder,
            commands::vault::rename_folder,
            commands::vault::delete_note,
            commands::vault::delete_attachment,
            commands::vault::copy_vault_file,
            commands::vault::copy_vault_folder,
            commands::vault::rename_attachment,
            commands::vault::read_attachment_data_url,
            // 历史侧文件迁移（笔记/表格/画布重命名/移动与文件夹改名后随迁历史版本）
            commands::vault::remap_sideloads,
            commands::vault::remap_sideloads_by_dir,
            // 多维表格（.atb）文件 CRUD
            commands::table::create_table_vault,
            commands::table::read_table_vault,
            commands::table::write_table_vault,
            commands::table::patch_table_vault,
            commands::table::rename_table_vault,
            commands::table::move_table_vault,
            commands::table::delete_table_vault,
            commands::table::import_table_image_vault,
            commands::table::export_table_xlsx,
            commands::table::save_image_to_downloads,
            // 全局配置（global.json，最近仓库列表等）
            commands::global::read_global_config,
            // 全局配置补丁（锁内读-合并-原子写，跨窗口并发不互相覆盖）
            commands::global::patch_global_config,
            // 本机设备名（协作身份默认值）
            commands::global::get_hostname,
            // API key 安全存储（OS keychain，见 commands/keychain.rs）
            commands::keychain::set_api_key,
            commands::keychain::get_api_key,
            commands::keychain::delete_api_key,
            // 通用应用秘密（协作令牌等，任意 name 隔离，见 commands/keychain.rs）
            commands::keychain::set_app_secret,
            commands::keychain::get_app_secret,
            commands::keychain::delete_app_secret,
            // 仓库文件检索（AI glob/grep 工具后端：模式发现路径 / 正则搜内容）
            commands::filesearch::glob_vault,
            commands::filesearch::grep_vault,
            // 联网搜索代理（Tavily/SearXNG，Rust 侧请求绕 CORS；key 与实例地址由前端配置层传入）
            commands::search::search_web,
            commands::web::fetch_web,
            commands::web::http_request,
            // 跨窗口拖拽释放检测（物理左键状态轮询，见 commands/windows.rs；Windows 专用净）
            commands::windows::is_mouse_left_down,
            // 打开文件上下文宿主（跨窗口协调态真源：get = 撕裂窗口 boot 基线，set = 主窗口写 + 广播）
            commands::open_context::get_open_file_context,
            commands::open_context::set_open_file_context,
            // 布局迷你窗口管理器（布局模型唯一权威：bootstrap/操作/非布局补丁/flush）
            layout::layout_bootstrap,
            layout::layout_op,
            layout::ui_state_patch,
            layout::layout_flush,
            // 跨窗口拖拽会话（源窗口上报输入；会话/命中调和/落点解析/看门狗在 Rust）
            layout::drag_update,
            layout::drag_hit,
            layout::drag_end,
            // 撕裂窗口生命周期（关闭上报移除条目 / 启动恢复调和）
            layout::panel_window_closed,
            layout::layout_reconcile,
            // 主页面板数据（日历/仓库历史：带日期笔记扫描 + 全仓库历史版本聚合）
            commands::home::list_dated_notes,
            commands::home::list_repo_history,
            // 插件平台（安装/卸载/启用/更新/读入口/插件数据/默认组合播种；见 commands/plugin.rs）
            commands::plugin::plugin_list,
            commands::plugin::plugin_assembly_version,
            commands::plugin::plugin_install,
            commands::plugin::plugin_install_local,
            commands::plugin::plugin_uninstall,
            commands::plugin::plugin_set_enabled,
            commands::plugin::plugin_apply_default_layout,
            commands::plugin::plugin_seed_default,
            commands::plugin::plugin_update,
            commands::plugin::plugin_rollback,
            commands::plugin::plugin_rebuild_local,
            commands::plugin::plugin_read_entry,
            commands::plugin::plugin_read_state,
            commands::plugin::plugin_write_state,
            commands::plugin::plugin_kv_read,
            commands::plugin::plugin_kv_set,
            commands::plugin::plugin_kv_delete,
            commands::plugin::plugin_kv_write,
            commands::external_fs::external_read_file,
            commands::external_fs::external_write_file,
            commands::external_fs::external_list_dir,
            commands::external_fs::external_create_folder,
            commands::external_fs::external_rename_file,
            commands::external_fs::external_move_file,
            commands::external_fs::external_delete_file,
            commands::external_fs::external_delete_dir,
            commands::external_fs::external_private_dir,
            commands::external_fs::external_write_file_base64,
            commands::external_fs::external_read_file_data_url,
            // 插件托管进程（ctx.process 的后端：启动 + stdin 写入/关闭 + 按 pid 结束进程树）
            commands::process::spawn_plugin_process,
            commands::process::write_plugin_process_stdin,
            commands::process::close_plugin_process_stdin,
            commands::process::kill_process_tree,
            // 全局快捷键（ctx.shortcuts 的 OS 层后端，见 commands/global_shortcut.rs）
            commands::global_shortcut::plugin_shortcut_register,
            commands::global_shortcut::plugin_shortcut_unregister,
            commands::global_shortcut::plugin_shortcut_release_plugin,
            commands::global_shortcut::plugin_shortcut_list,
            // 应用内更新下载与安装（进度 / 取消 / 断点续传 / 摘要校验；见 commands/update.rs）
            commands::update::download_update_package,
            commands::update::cancel_update_download,
            commands::update::install_downloaded_update,
            // 系统托盘（图标/菜单、驻留显隐与完全退出协调，见 tray.rs；移动端无托盘语义）
            tray::hide_to_tray,
            tray::exit_flush_done,
            tray::tray_set_plugin_menu,
            // 屏幕与显示器几何（插件多窗口定位用；移动端单屏返回空列表）
            commands::windows::list_monitors,
            // 移动端专属（安卓本地仓库：存储权限、私有回落目录、自研目录浏览；桌面端一律拒绝）
            commands::mobile::android_has_all_files_access,
            commands::mobile::android_request_all_files_access,
            commands::mobile::android_open_url,
            commands::mobile::android_can_install_packages,
            commands::mobile::android_request_install_permission,
            commands::mobile::android_install_apk,
            commands::mobile::android_private_vault_path,
            commands::mobile::android_storage_root,
            commands::mobile::list_absolute_dir,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        // 应用退出：结束全部插件托管进程（进程不该活过应用）。走 Exit 而非 ExitRequested——
        // 退出请求在关窗守卫里可能被拦下，Exit 才是终态（窗口已全销毁、不再有新进程启动）。
        // 崩溃与强杀不经过这里，由 Windows 作业对象随句柄关闭兜底（见 plugin_process.rs）。
        if let tauri::RunEvent::Exit = event {
            plugin_process::shutdown(handle);
        }
    });
}

#[cfg(test)]
mod capability_contract_tests {
    /// 启动底色契约：原生窗口刷与主题 `--bg-primary` 必须逐档同值——`tauri.conf.json`（建窗时的
    /// 深色缺省，见 lib.rs setup 的按主题改底色）、`commands/windows.rs`（深浅两档常量）、
    /// `index.html`（首帧按 `.dark` 分支取）任一处异值都会在启动时闪一次异色。
    #[test]
    fn startup_background_matches_theme_primary() {
        let css = include_str!("../../src/styles/index.css");
        // 取指定主题块里的 --bg-primary（`:root {` 是浅色基础块，`:root.dark {` 是深色块）
        let primary_of = |selector: &str| -> String {
            let at = css
                .find(selector)
                .unwrap_or_else(|| panic!("index.css 缺少 {selector} 块"));
            let block = &css[at..];
            let block = &block[..block.find('}').expect("主题块未闭合")];
            block
                .split("--bg-primary:")
                .nth(1)
                .expect("主题块缺少 --bg-primary")
                .split(';')
                .next()
                .unwrap()
                .trim()
                .to_lowercase()
        };
        let dark = primary_of(":root.dark {");
        let light = primary_of(":root {");
        assert!(dark.starts_with('#') && light.starts_with('#'), "两套 --bg-primary 应为 hex 字面量");

        let html = include_str!("../../index.html").to_lowercase();
        for (name, bg) in [("深色", &dark), ("浅色", &light)] {
            assert!(
                html.contains(&format!("background:{bg}")),
                "index.html 的{name}首帧底色与 --bg-primary（{bg}）不一致"
            );
        }

        let cfg: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json 解析失败");
        let window_bg = cfg["app"]["windows"][0]["backgroundColor"]
            .as_str()
            .expect("app.windows[0].backgroundColor 未配置")
            .to_lowercase();
        assert_eq!(window_bg, dark, "主窗口配置底色应为深色主题底色（浅色由 setup 按主题改）");

        // 两极底色常量：两档都要在 windows.rs 里，缺浅色档 = 浅色主题仍闪深色
        let as_literal = |hex: &str| {
            let rgb = |i: usize| u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).unwrap();
            format!("Color({}, {}, {}, 255)", rgb(0), rgb(1), rgb(2))
        };
        let windows_rs = include_str!("commands/windows.rs");
        for (name, hex) in [("深色", &dark), ("浅色", &light)] {
            let expected = as_literal(hex);
            assert!(
                windows_rs.contains(&expected),
                "启动底色常量缺{name}档 {expected}（应与 --bg-primary 同值）"
            );
        }
    }

    /// 主题解析口径契约：原生底色按 `global.json` 的主题解析（见 `commands/global.rs`），
    /// 这里的键名与内置主题插件 id 必须与前端一致——不一致只会让浅色主题的用户仍旧闪深色，
    /// 静默失效且运行时无断言点。
    #[test]
    fn startup_theme_scheme_keys_match_frontend() {
        let frontend = include_str!("../../src/utils/pluginTheme.ts");
        assert!(
            frontend.contains(r#"BUILTIN_THEME_PLUGIN_ID = "builtin.theme""#),
            "前端内置主题插件 id 已变"
        );
        assert!(frontend.contains(r#"COLOR_MODE_KEY = "colorMode""#), "前端 colorMode 键已变");
        assert!(frontend.contains(r#"VARIANT_KEY = "variant""#), "前端 variant 键已变");

        let rust = include_str!("commands/global.rs");
        assert!(rust.contains(r#"BUILTIN_THEME_PLUGIN_ID: &str = "builtin.theme""#));
        assert!(rust.contains(r#"THEME_COLOR_MODE_KEY: &str = "colorMode""#));
        assert!(rust.contains(r#"THEME_VARIANT_KEY: &str = "variant""#));
    }

    /// 校验 `tauri.conf.json > plugins > shell > open` 的放行范围。
    ///
    /// tauri-plugin-shell 会把该正则**整体包上 `^...$`** 后逐项 `is_match`，
    /// 因此配置里的正则本身不得再写 `^`/`$`，且分支必须自带「前缀 + 余下部分」结构。
    /// 放行：http(s)/file/mailto/tel/xmpp 与本地绝对路径（盘符、UNC、Unix `/`，含裸根）；
    /// 拒绝：相对路径、未知 scheme 与命令行风格的 `-`/`--` 开头串。
    #[test]
    fn open_scope_allows_local_paths_and_known_schemes() {
        let cfg: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json 解析失败");
        let validator = cfg["plugins"]["shell"]["open"]
            .as_str()
            .expect("plugins.shell.open 未配置");
        let regex = regex::Regex::new(&format!("^{validator}$")).expect("shell.open 正则非法");

        for allowed in [
            "https://example.com/x",
            "http://127.0.0.1:8080/a?b=c",
            "file:///home/u/a.md",
            "mailto:a@b.com",
            "tel:+8613800138000",
            "xmpp:a@b.com",
            "E:\\仓库",
            "E:/仓库",
            "C:\\Users\\me\\Desktop",
            "\\\\server\\share\\x",
            "/home/u/仓库/笔记.md",
            // 裸根：在文件管理器中打开盘符/文件系统根
            "/",
            "C:\\",
            "E:/",
            "\\\\",
        ] {
            assert!(regex.is_match(allowed), "应放行：{allowed}");
        }

        for denied in [
            "",
            "-i",
            "--enable-debugging",
            "relative/x.md",
            "javascript:alert(1)",
            "data:text/html,x",
            "vbscript:msgbox(1)",
            "ftp://x.com/a",
        ] {
            assert!(!regex.is_match(denied), "应拒绝：{denied}");
        }
    }

    /// 插件进程执行走宿主 `spawn_plugin_process`（程序来源校验在 Rust 侧 `plugin_process::resolve_program`），
    /// 为的是在进程创建那一刻就定下清理归属。故 `shell:allow-spawn`/`allow-execute`/`allow-kill` 不得
    /// 出现在任何能力文件里：那等于敞开一条不进程记账、不随应用退出的执行路径。
    ///
    /// 扫整个目录而不是点名某几个文件：换个文件名重新登记 scope 同样是敞开了那条路径。
    #[test]
    fn no_shell_process_capabilities() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).expect("capabilities 目录读取失败") {
            let path = entry.expect("capabilities 目录项读取失败").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            scanned += 1;
            let cfg: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("能力文件读取失败"))
                    .expect("能力文件解析失败");
            let Some(perms) = cfg["permissions"].as_array() else {
                continue;
            };
            for perm in perms {
                // 字符串与带 scope 的对象两种写法都要看
                let id = perm
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| perm["identifier"].as_str().map(str::to_string));
                for must_not in ["shell:allow-execute", "shell:allow-spawn", "shell:allow-kill"] {
                    assert_ne!(
                        id.as_deref(),
                        Some(must_not),
                        "{} 不该出现 {must_not}：插件进程执行走宿主 spawn_plugin_process",
                        path.display()
                    );
                }
            }
        }
        assert!(scanned > 0, "capabilities 目录下没有扫到任何能力文件");
    }

    /// `shell:default` 只含 `allow-open`（本地路径靠 `plugins.shell.open` 的 scope 放行）。
    #[test]
    fn default_capability_keeps_shell_open_only() {
        let cfg: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).expect("capabilities 解析失败");
        let perms: Vec<String> = cfg["permissions"]
            .as_array()
            .expect("permissions 缺失")
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v["identifier"].as_str().map(str::to_string))
            })
            .collect();
        assert!(perms.iter().any(|p| p == "shell:default"), "缺少 shell:default（open 依赖）");
    }

    /// 打包版 CSP（tauri.conf.json）与开发版 CSP（index.html meta）以追加方式叠加取交集，
    /// 任一指令只写一处都会在另一形态下收窄。契约：两处的指令集合必须逐条一致。
    #[test]
    fn csp_directives_match_between_config_and_index_html() {
        let parse = |raw: &str| -> std::collections::BTreeMap<String, String> {
            raw.split(';')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(|d| {
                    let (name, sources) = d.split_once(' ').unwrap_or((d, ""));
                    (name.to_string(), sources.trim().to_string())
                })
                .collect()
        };
        let cfg: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json 解析失败");
        let config_csp = cfg["app"]["security"]["csp"].as_str().expect("app.security.csp 未配置");
        let html = include_str!("../../index.html");
        let anchor = html
            .find("http-equiv=\"Content-Security-Policy\"")
            .expect("index.html 缺少 CSP meta");
        let rest = &html[anchor..];
        let content_start = rest.find("content=\"").expect("CSP meta 缺少 content") + "content=\"".len();
        let content_end = rest[content_start..].find('"').expect("CSP content 未闭合");
        let html_csp = &rest[content_start..content_start + content_end];

        let config_map = parse(config_csp);
        let html_map = parse(html_csp);
        assert_eq!(
            config_map.keys().collect::<Vec<_>>(),
            html_map.keys().collect::<Vec<_>>(),
            "两处 CSP 的指令集合不一致"
        );
        for (name, sources) in &config_map {
            assert_eq!(
                sources.split_whitespace().collect::<Vec<_>>(),
                html_map[name].split_whitespace().collect::<Vec<_>>(),
                "CSP 指令 {name} 的源列表在两处不一致"
            );
        }
    }
}
