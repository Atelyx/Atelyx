//! 插件整面帧的共享存储与窗口红绘请求：订阅流写入，绘制期直接读最新帧。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};

use atelyx_core::plugin_runtime::SurfaceFrame;

/// 按插件 id / 面 id 保存最新整面帧。帧提交后由订阅流直接写入，绘制期读它，
/// 不再经 update 投影触发整窗视图重建。
#[derive(Clone, Default)]
pub struct FrameStore(Arc<RwLock<HashMap<String, HashMap<String, Arc<SurfaceFrame>>>>>);

impl FrameStore {
    pub fn get(&self, plugin_id: &str, surface_id: &str) -> Option<Arc<SurfaceFrame>> {
        self.read().get(plugin_id)?.get(surface_id).cloned()
    }

    /// 写入一帧；返回该面此前是否没有帧（首次到达需要壳侧重建一次视图换取整面部件）。
    pub fn insert(&self, plugin_id: &str, surface_id: &str, frame: Arc<SurfaceFrame>) -> bool {
        let mut store = self.write();
        let surfaces = store.entry(plugin_id.to_string()).or_default();
        let first = !surfaces.contains_key(surface_id);
        surfaces.insert(surface_id.to_string(), frame);
        first
    }

    /// 移除一帧；返回此前是否有帧（有才需要壳侧补一次投影）。
    pub fn remove(&self, plugin_id: &str, surface_id: &str) -> bool {
        let mut store = self.write();
        let Some(surfaces) = store.get_mut(plugin_id) else { return false };
        let removed = surfaces.remove(surface_id).is_some();
        if surfaces.is_empty() {
            store.remove(plugin_id);
        }
        removed
    }

    /// 清掉一个插件的全部帧（停用 / 会话收尾）。
    pub fn clear_plugin(&self, plugin_id: &str) {
        self.write().remove(plugin_id);
    }

    /// 整体替换为给定帧表（布局变化后按已挂面重取，只保留仍在承载点上的面）。
    pub fn replace(&self, frames: HashMap<(String, String), Arc<SurfaceFrame>>) {
        let mut store: HashMap<String, HashMap<String, Arc<SurfaceFrame>>> = HashMap::new();
        for ((plugin_id, surface_id), frame) in frames {
            store.entry(plugin_id).or_default().insert(surface_id, frame);
        }
        *self.write() = store;
    }

    /// 读锁遍历（图片预载 / 模态浮层重算）：锁只在遍历期间持有，写回 App 状态前须先释放。
    pub fn read(
        &self,
    ) -> RwLockReadGuard<'_, HashMap<String, HashMap<String, Arc<SurfaceFrame>>>> {
        self.0.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, HashMap<String, Arc<SurfaceFrame>>>> {
        self.0.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 帧到达后的窗口红绘请求。Windows 直接向窗口投一次 `WM_PAINT`（winit 转成
/// RedrawRequested，只重绘既有部件树，不跑 `App::view`）；其它平台没有等价的
/// 外部唤醒入口，订阅流沿用批量消息。
#[derive(Clone, Default)]
pub struct FrameRedraw {
    /// 各窗口的原生句柄（Windows 上即 HWND 数值）。
    windows: Arc<Mutex<Vec<u64>>>,
}

impl FrameRedraw {
    pub fn register(&self, raw_window: u64) {
        let mut windows = self.windows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !windows.contains(&raw_window) {
            windows.push(raw_window);
        }
    }

    pub fn forget(&self, raw_window: u64) {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|window| *window != raw_window);
    }

    /// 请求各窗口重绘。只有 `WM_PAINT` 不整窗失效：整窗内容每帧由渲染器重画，
    /// 失效区域没有意义；多次请求由系统合并。
    #[cfg(target_os = "windows")]
    pub fn request(&self) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INTERNALPAINT};

        let windows = self.windows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for raw in windows.iter() {
            // 句柄来自已打开窗口，窗口销毁后由 `forget` 摘除；此处调用失败只说明窗口正在关闭
            unsafe {
                let _ = RedrawWindow(Some(HWND(*raw as *mut _)), None, None, RDW_INTERNALPAINT);
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn request(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::plugin_runtime::SurfaceFrame;

    fn frame() -> Arc<SurfaceFrame> {
        Arc::new(SurfaceFrame::default())
    }

    #[test]
    fn insert_reports_first_only_once() {
        let store = FrameStore::default();
        assert!(store.insert("p", "s", frame()), "首次写入要壳侧补投影");
        assert!(!store.insert("p", "s", frame()), "同面换帧不再触发视图重建");
    }

    #[test]
    fn remove_and_clear_drop_keys() {
        let store = FrameStore::default();
        store.insert("p", "s", frame());
        store.insert("p", "other", frame());
        store.insert("q", "s", frame());
        assert!(store.remove("p", "s"));
        assert!(!store.remove("p", "s"), "已移除的键不再报告");
        assert!(store.get("p", "other").is_some());
        store.clear_plugin("p");
        assert!(store.get("p", "other").is_none());
        assert!(store.get("q", "s").is_some(), "只清指定插件");
    }

    #[test]
    fn replace_keeps_only_given_frames() {
        let store = FrameStore::default();
        store.insert("p", "gone", frame());
        let mut frames = HashMap::new();
        frames.insert(("p".to_string(), "kept".to_string()), frame());
        store.replace(frames);
        assert!(store.get("p", "kept").is_some());
        assert!(store.get("p", "gone").is_none());
    }
}
