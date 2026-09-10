//! Atelyx Tauri 后端入口。
//! 负责命令注册与仓库/watcher 状态托管。
//!
//! 文件化仓库（`vault.rs`）为唯一存储出口。

mod commands;
mod layout;
mod layout_drag;
mod layout_model;
mod layout_persist;
mod layout_window;
mod vault;
mod watcher;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // 自动检查更新（tauri-plugin-updater，endpoints/pubkey 见 tauri.conf.json）
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            // 仓库化：注册当前仓库根路径状态（初始为 None，open_vault 时设置）
            app.manage(vault::VaultState::default());
            // 文件监听：持有当前仓库的 notify debouncer，open_vault 时启动
            app.manage(watcher::WatcherState::default());
            // 布局迷你窗口管理器：布局模型唯一权威，启动即从 ui-state.json 加载
            app.manage(layout::LayoutState::new());
            layout::load_from_disk(app.handle(), &app.state::<layout::LayoutState>());
            // 主窗口窗口事件钩子：Moved/Resized → 权威 bounds（拖拽命中/落点解析）
            if let Some(main_win) = app.get_webview_window("main") {
                main_win.on_window_event(layout::window_event_handler(app.handle(), "main".into()));
                // 种子化初始 bounds：启动后未移动过时 on_window_event 不触发，拖拽解析读不到
                layout::seed_window_bounds(app.handle(), "main");
            }
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
            commands::vault::write_vault_config,
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
            commands::vault::ensure_default_vault,
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
            commands::table::cleanup_table_attachments_vault,
            commands::table::export_table_xlsx,
            commands::table::save_image_to_downloads,
            // 全局配置（global.json，最近仓库列表等）
            commands::global::read_global_config,
            commands::global::write_global_config,
            // 本机设备名（协作身份默认值）
            commands::global::get_hostname,
            // API key 安全存储（OS keychain，见 commands/keychain.rs）
            commands::keychain::set_api_key,
            commands::keychain::get_api_key,
            commands::keychain::delete_api_key,
            // 仓库文件检索（AI glob/grep 工具后端：模式发现路径 / 正则搜内容）
            commands::filesearch::glob_vault,
            commands::filesearch::grep_vault,
            // 联网搜索代理（Tavily/SearXNG，Rust 侧请求绕 CORS + key 不进 WebView）
            commands::search::search_web,
            commands::web::fetch_web,
            // 跨窗口拖拽释放检测（物理左键状态轮询，见 commands/windows.rs；Windows 专用净）
            commands::windows::is_mouse_left_down,
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
            commands::plugin::plugin_install,
            commands::plugin::plugin_install_local,
            commands::plugin::plugin_uninstall,
            commands::plugin::plugin_set_enabled,
            commands::plugin::plugin_seed_default,
            commands::plugin::plugin_update,
            commands::plugin::plugin_read_entry,
            commands::plugin::plugin_read_state,
            commands::plugin::plugin_write_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
