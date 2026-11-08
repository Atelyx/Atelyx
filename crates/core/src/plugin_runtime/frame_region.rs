//! 整面帧共享内存区域：宿主创建临时文件、铺区域头与槽头，插件映射后写入，宿主只经信封拿到
//! 槽位坐标再回读解码。扩容换新文件并递增代次；旧文件在映射释放后删除，删不掉留待重试。
//! 应用启动时清一次超过陈旧阈值的遗留文件（删除路径不影响已映射的实例）。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atelyx_plugin_protocol::frame::FrameSubmitSharedParams;
use atelyx_plugin_protocol::frame_region::{
    self, FrameRegionInfo, MAX_SLOT_CAPACITY, REGION_HEADER_SIZE, SLOT_CAPACITY,
};

use memmap2::{Mmap, MmapOptions};

/// 临时文件前缀：启动清扫只认这个前缀。
const TEMP_PREFIX: &str = "atelyx-frame-";
/// 启动清扫的陈旧阈值：覆盖正常重启的时间跨度，又不在本次运行内误删仍在用的文件。
const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

/// 一份已映射的区域（只读）：区域头已初始化，内容由插件进程写入。
struct FrameRegion {
    path: PathBuf,
    _file: File,
    map: Mmap,
    info: FrameRegionInfo,
}

impl FrameRegion {
    fn create(plugin_id: &str, generation: u64, slot_capacity: u32) -> Result<Self, String> {
        let info = FrameRegionInfo {
            path: String::new(),
            generation,
            ..FrameRegionInfo::new_layout(slot_capacity)
        };
        let path = temp_region_path(plugin_id, generation);
        let path_text = path
            .to_str()
            .ok_or_else(|| format!("帧区域路径不是有效 UTF-8：{}", path.display()))?
            .to_string();
        let file = open_private(&path).map_err(|e| format!("创建帧区域文件失败：{e}"))?;
        let len = info.byte_len();
        let result = (|| {
            file.set_len(len as u64)
                .map_err(|e| format!("设置帧区域长度失败：{e}"))?;
            let mut map = unsafe { MmapOptions::new().len(len).map_mut(&file) }
                .map_err(|e| format!("映射帧区域失败：{e}"))?;
            frame_region::init_region(&mut map, &info)?;
            // 只刷区域头：槽位从全零起步，无需把整份文件（最大 64 MiB）写成脏页再回写
            map.flush_range(0, REGION_HEADER_SIZE)
                .map_err(|e| format!("刷写帧区域头失败：{e}"))?;
            drop(map);
            let map = unsafe { MmapOptions::new().len(len).map(&file) }
                .map_err(|e| format!("映射帧区域失败：{e}"))?;
            let info = FrameRegionInfo { path: path_text, ..info };
            Ok(Self { path: path.clone(), _file: file, map, info })
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    /// 复制这一笔提交的槽内容；序号对不上（写入中被覆盖）返回 `None`，交由调用方丢弃。
    fn read(&self, params: &FrameSubmitSharedParams) -> Result<Option<Vec<u8>>, String> {
        if params.generation != self.info.generation {
            return Err(format!(
                "帧区域代次不符：提交 {}，当前 {}",
                params.generation, self.info.generation
            ));
        }
        frame_region::read_slot(&self.map, &self.info, params.slot, params.seq, params.len)
    }

    fn info(&self) -> FrameRegionInfo {
        self.info.clone()
    }
}

/// 一个插件的帧区域：随会话创建、可扩容，会话收尾释放。
pub(crate) struct FrameRegionStore {
    plugin_id: String,
    generation: AtomicU64,
    current: Mutex<Option<FrameRegion>>,
    retired: Mutex<Vec<PathBuf>>,
}

impl FrameRegionStore {
    pub(crate) fn new(plugin_id: &str) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            generation: AtomicU64::new(0),
            current: Mutex::new(None),
            retired: Mutex::new(Vec::new()),
        }
    }

    /// 取当前区域描述；尚未创建即建一份。
    pub(crate) fn ensure(&self) -> Result<FrameRegionInfo, String> {
        let mut current = self.current.lock().unwrap();
        if let Some(region) = current.as_ref() {
            return Ok(region.info());
        }
        let region = FrameRegion::create(&self.plugin_id, self.next_generation(), SLOT_CAPACITY)?;
        let info = region.info();
        *current = Some(region);
        self.retry_retired();
        Ok(info)
    }

    /// 换一份至少能放下 `min_bytes` 的区域并返回新描述；容量已够时原样返回当前描述。
    pub(crate) fn grow(&self, min_bytes: u32) -> Result<FrameRegionInfo, String> {
        let mut current = self.current.lock().unwrap();
        let Some(region) = current.as_ref() else {
            return Err("帧区域尚未创建".into());
        };
        if min_bytes > MAX_SLOT_CAPACITY {
            return Err(format!(
                "帧过大：{min_bytes} 字节超过单帧上限 {MAX_SLOT_CAPACITY} 字节"
            ));
        }
        if region.info.slot_capacity >= min_bytes {
            return Ok(region.info());
        }
        let mut capacity = region.info.slot_capacity;
        while capacity < min_bytes {
            capacity = capacity.saturating_mul(2).min(MAX_SLOT_CAPACITY);
        }
        let new = FrameRegion::create(&self.plugin_id, self.next_generation(), capacity)?;
        let info = new.info();
        let old = current.replace(new);
        drop(current);
        if let Some(old) = old {
            let path = old.path.clone();
            drop(old);
            self.retire(path);
        }
        self.retry_retired();
        Ok(info)
    }

    /// 复制一笔提交的槽内容：区域未建时按「无此区域」报错，不让提交悄悄落空。
    /// 解码与校验由调用方在会话闸门外完成，锁只覆盖拷贝。
    pub(crate) fn read(&self, params: &FrameSubmitSharedParams) -> Result<Option<Vec<u8>>, String> {
        let current = self.current.lock().unwrap();
        let Some(region) = current.as_ref() else {
            return Err("帧区域尚未创建".into());
        };
        region.read(params)
    }

    /// 会话收尾：释放映射并删文件；删不掉留待下次重试。
    pub(crate) fn release(&self) {
        let region = self.current.lock().unwrap().take();
        if let Some(region) = region {
            let path = region.path.clone();
            drop(region);
            self.retire(path);
        }
        self.retry_retired();
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn retire(&self, path: PathBuf) {
        if std::fs::remove_file(&path).is_err() {
            self.retired.lock().unwrap().push(path);
        }
    }

    fn retry_retired(&self) {
        let mut retired = self.retired.lock().unwrap();
        retired.retain(|path| std::fs::remove_file(path).is_err());
    }
}

fn open_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn temp_region_path(plugin_id: &str, generation: u64) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let owner: String = plugin_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{TEMP_PREFIX}{}-{owner}-{generation}-{nanos}-{serial}.bin",
        std::process::id()
    ))
}

/// 清理上次异常退出留下的区域文件：超出陈旧阈值的才删；删除路径不影响已映射的实例。
pub(crate) fn sweep_stale_regions() {
    std::thread::spawn(|| {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(TEMP_PREFIX) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let stale = meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > STALE_AFTER);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::frame::SurfaceFrame;
    use atelyx_plugin_protocol::frame_codec;
    use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};

    fn sample_frame(content: &str) -> SurfaceFrame {
        SurfaceFrame {
            commands: vec![DrawCommand::Text {
                x: 0.0,
                y: 0.0,
                content: content.to_string(),
                size: 13.0,
                color: PaintColor::Token { name: ColorToken::Text },
                weight: None,
                family: None,
                max_width: None,
                align: None,
            }],
            ..SurfaceFrame::default()
        }
    }

    /// 按插件进程的方式写槽：打开同一份文件做可写映射，走同一套槽位布局。
    fn write_frame(info: &FrameRegionInfo, slot: u32, seq: u64, frame: &SurfaceFrame) -> u32 {
        let file = OpenOptions::new().read(true).write(true).open(&info.path).unwrap();
        let mut map = unsafe { MmapOptions::new().len(info.byte_len()).map_mut(&file).unwrap() };
        let payload = frame_codec::encode(frame);
        frame_region::write_slot(&mut map, info, slot, seq, &payload).unwrap();
        map.flush().unwrap();
        payload.len() as u32
    }

    fn locator(info: &FrameRegionInfo, slot: u32, seq: u64, len: u32) -> FrameSubmitSharedParams {
        FrameSubmitSharedParams {
            surface_id: "panel".into(),
            generation: info.generation,
            slot,
            seq,
            len,
        }
    }

    #[test]
    fn written_slot_reads_back_and_stale_or_mismatched_locators_are_rejected() {
        let store = FrameRegionStore::new("com.atelyx.test");
        let info = store.ensure().unwrap();
        assert!(Path::new(&info.path).is_file(), "区域文件按描述落盘");
        assert_eq!(info.slot_capacity, SLOT_CAPACITY);

        let frame = sample_frame("面板");
        let len = write_frame(&info, 1, 2, &frame);
        let bytes = store.read(&locator(&info, 1, 2, len)).unwrap().expect("槽内容应可读");
        assert_eq!(frame_codec::decode(&bytes).unwrap(), frame);
        assert_eq!(store.read(&locator(&info, 1, 4, len)).unwrap(), None, "序号对不上按写入中丢弃");
        assert!(store.read(&locator(&info, 1, 2, len + 1)).is_err(), "槽内长度与提交不符");
        assert!(store.read(&locator(&info, 9, 2, len)).is_err(), "槽位越界");
        assert!(store.read(&locator(&info, 1, 2, SLOT_CAPACITY + 1)).is_err(), "长度越界");
        let mut stale = locator(&info, 1, 2, len);
        stale.generation += 1;
        assert!(store.read(&stale).is_err(), "过期代次不得读取");
    }

    #[test]
    fn growth_swaps_the_file_and_retires_the_old_generation() {
        let store = FrameRegionStore::new("com.atelyx.test");
        let first = store.ensure().unwrap();
        let doubled = SLOT_CAPACITY * 2;
        let grown = store.grow(doubled).unwrap();
        assert_eq!(grown.slot_capacity, doubled);
        assert_eq!(grown.generation, first.generation + 1);
        assert_ne!(grown.path, first.path);
        assert!(!Path::new(&first.path).exists(), "旧文件在映射释放后删除");
        assert!(store.read(&locator(&first, 0, 2, 0)).is_err(), "过期代次不再读取");

        let same = store.grow(SLOT_CAPACITY).unwrap();
        assert_eq!(same.generation, grown.generation, "容量够时原样返回当前区域");
        assert!(store.grow(MAX_SLOT_CAPACITY + 1).is_err(), "超过单帧上限即报错");
    }

    #[test]
    fn release_removes_the_region_file() {
        let store = FrameRegionStore::new("com.atelyx.test");
        let info = store.ensure().unwrap();
        store.release();
        assert!(!Path::new(&info.path).exists());
        assert!(store.read(&locator(&info, 0, 2, 0)).is_err(), "释放后不再读取");
    }
}
