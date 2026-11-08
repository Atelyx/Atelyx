//! 界面字体：字体族取自系统已安装字体，应用不分发字体文件。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{OnceLock, RwLock};

use iced::advanced::text::IntoFragment;

/// 一个已安装字体族：展示名 + 该族全部语言别名（已小写）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyEntry {
    /// 展示名（有汉字记录时用汉字名，否则用首选记录；也是向字体库要字体的匹配键）。
    pub name: String,
    /// 该族全部语言别名（含中文名等，已小写），供设置页过滤与族名匹配。
    pub aliases: Vec<String>,
}

/// 已安装族清单：设置页选项 + 族名校验，也是族名拿到 `&'static str`
/// （`iced::font::Family::Name` 的要求）的唯一来源——未安装的族名不驻留，
/// 免得用户任意输入的字符串把内存漏进字体族。
static FAMILIES: OnceLock<Vec<FamilyEntry>> = OnceLock::new();
/// 别名（小写）→ 展示名：族名匹配口径与字体库一致（命中该族任一别名，大小写无关），
/// 且把表格绘制路径上的每帧查表降到一次哈希。
static ALIASES: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
/// 跟随系统时生效的族名（None = 交给通用无衬线族）。
static SYSTEM_FAMILY: OnceLock<Option<&'static str>> = OnceLock::new();
/// 用户选定的族名与变更计数（两者必须同步变：计数是只按尺寸失效的自绘缓存的唯一作废依据）。
static CHOICE: RwLock<Choice> = RwLock::new(Choice { family: None, epoch: 0 });

struct Choice {
    /// 选定的族名（None = 跟随系统）。
    family: Option<&'static str>,
    /// 换族次数（启动为 0，每次实际换族 +1）。
    epoch: u64,
}

impl Choice {
    fn set(&mut self, resolved: Option<&'static str>) {
        if self.family != resolved {
            self.family = resolved;
            self.epoch += 1;
        }
    }
}

/// 扫描已安装族并解析跟随系统的族名；只在启动阶段（渲染通道外）调用一次。
/// 这里取字体系统写锁，绘制路径已持同一把锁，重入即死锁。
pub fn init() {
    let _ = FAMILIES.set(scan_families());
    let _ = ALIASES.set(alias_index(entries()));
    let _ = SYSTEM_FAMILY.set(sys::ui_family().and_then(|name| lookup(&name)));
}

/// 当前生效的界面字体族：设置指定且已安装时用该族，否则用系统 UI 字体，都取不到时用通用无衬线族。
pub fn family() -> iced::font::Family {
    match active_family() {
        Some(name) => iced::font::Family::Name(name),
        None => iced::font::Family::SansSerif,
    }
}

fn active_family() -> Option<&'static str> {
    let chosen = CHOICE.read().expect("读取界面字体族").family;
    chosen.or(*SYSTEM_FAMILY.get()?)
}

/// 指定界面字体族（None = 跟随系统）；未安装的族名按跟随系统处理。
pub fn set_family(name: Option<&str>) {
    let resolved = name.and_then(lookup);
    CHOICE.write().expect("写入界面字体族").set(resolved);
}

/// 界面字体族变更计数（启动为 0，每次实际换族 +1）。
pub fn epoch() -> u64 {
    CHOICE.read().expect("读取界面字体族").epoch
}

/// 族名是否已安装（供设置行如实标注跨机搬来的旧值）。
pub fn is_installed(name: &str) -> bool {
    lookup(name).is_some()
}

/// 族名 → 清单里的驻留名：命中该族任一语言别名即可（与字体库的族匹配同口径，大小写无关）。
pub fn lookup(name: &str) -> Option<&'static str> {
    ALIASES.get()?.get(&name.to_lowercase()).copied()
}

/// 已安装族清单（启动扫描结果，已按展示名排序）。
fn entries() -> &'static [FamilyEntry] {
    FAMILIES.get().map(Vec::as_slice).unwrap_or(&[])
}

/// 按关键字过滤族清单（空查询 = 全量；命中展示名或该族任一语言别名）。
pub fn filter(query: &str) -> Vec<&'static FamilyEntry> {
    matching_entries(entries(), query)
}

fn matching_entries<'a>(all: &'a [FamilyEntry], query: &str) -> Vec<&'a FamilyEntry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return all.iter().collect();
    }
    all.iter()
        .filter(|entry| entry.aliases.iter().any(|alias| alias.contains(&needle)))
        .collect()
}

/// 正文字重。
pub fn regular() -> iced::Font {
    iced::Font { family: family(), ..iced::Font::default() }
}

/// 强调字重：系统字体家族通常只有 400/700 两档，中量级（500/600）请求会被匹配成
/// 400（看不出强调）或 700，因此全应用的强调统一取 700。
pub fn bold() -> iced::Font {
    iced::Font {
        family: family(),
        weight: iced::font::Weight::Bold,
        ..iced::Font::default()
    }
}

/// 行内斜体（当前界面族 + Italic）。
pub fn italic() -> iced::Font {
    iced::Font {
        family: family(),
        style: iced::font::Style::Italic,
        ..iced::Font::default()
    }
}

/// 等宽字体：始终取系统通用等宽族，不随界面字体族设置变化。
pub fn mono() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Monospace,
        ..iced::Font::default()
    }
}

/// 界面文字：构造时就带上当前字体族，调用方与 `iced::widget::text` 同签名。
///
/// iced 的 `default_font` 只在创建渲染器时读一次，运行时改字体族对没显式传字体的文字无效，
/// 所以全应用的文字都从这里出，强调处再按 [`bold`] / [`italic`] 覆盖字重与字形。
pub fn text<'a>(content: impl IntoFragment<'a>) -> iced::widget::Text<'a> {
    iced::widget::text(content).font(regular())
}

/// 扫描系统已安装字体族（字体库没有 face 时得到空清单，界面回落通用无衬线族）。
fn scan_families() -> Vec<FamilyEntry> {
    let mut guard = iced::advanced::graphics::text::font_system().write().expect("写入字体系统");
    let faces: Vec<Vec<String>> = guard
        .raw()
        .db()
        .faces()
        .map(|face| prefer_display(face.families.iter().map(|(name, _)| name.clone()).collect()))
        .collect();
    drop(guard);
    family_entries(&faces)
}

/// 把适合中文界面的族名挪到最前当展示名，其余语言记录留在后面并入别名。
/// 界面文案是中文，「宋体 / 微软雅黑」比「SimSun / Microsoft YaHei」好认。
fn prefer_display(mut names: Vec<String>) -> Vec<String> {
    if let Some(i) = names.iter().position(|name| is_han_display(name)) {
        names.swap(0, i);
    }
    names
}

/// 族名可否当展示名：含汉字即可，混进假名或谚文的除外（日文字体自带名如「游ゴシック」，
/// 中文界面里不如它的英文名好认）。
fn is_han_display(name: &str) -> bool {
    let mut han = false;
    for c in name.chars() {
        match c {
            '\u{3400}'..='\u{9fff}' | '\u{F900}'..='\u{FAFF}' => han = true,
            '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}' => return false,
            _ => {}
        }
    }
    han
}

/// 从各字体面的族名别名归并出族条目：同族去重、展示名取序列首别名、别名并入全部语言变体。
fn family_entries(faces: &[Vec<String>]) -> Vec<FamilyEntry> {
    // 归族键 = 展示名别名的小写：同一族在不同字体文件里大小写不同的展示名（ubuntu / Ubuntu）只留一条，
    // 且 BTreeMap 按该键迭代 = 拉丁名按字母序在前、汉字名成块在后。
    let mut by_name: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for aliases in faces {
        let Some(first) = aliases.first() else {
            continue;
        };
        let entry = by_name
            .entry(first.to_lowercase())
            .or_insert_with(|| (first.clone(), BTreeSet::new()));
        entry.1.extend(aliases.iter().map(|alias| alias.to_lowercase()));
    }
    by_name
        .into_values()
        .map(|(name, aliases)| FamilyEntry { name, aliases: aliases.into_iter().collect() })
        .collect()
}

/// 别名（小写）→ 展示名索引：一个别名只归属一个族（清单按展示名排序，先到者占位）。
fn alias_index<'a>(all: &'a [FamilyEntry]) -> HashMap<String, &'a str> {
    let mut index: HashMap<String, &'a str> = HashMap::new();
    for entry in all {
        for alias in &entry.aliases {
            index.entry(alias.clone()).or_insert(entry.name.as_str());
        }
    }
    index
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::UI::WindowsAndMessaging::{
        NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };

    /// Windows 的非客户区度量里的消息字体就是外壳与各原生应用共用的 UI 字体。
    pub fn ui_family() -> Option<String> {
        let mut metrics = NONCLIENTMETRICSW::default();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let fetched = unsafe {
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                0,
                Some(std::ptr::addr_of_mut!(metrics).cast::<c_void>()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        fetched.ok()?;
        face_name(&metrics.lfMessageFont.lfFaceName)
    }

    /// UTF-16 定长缓冲区 → 族名（遇到第一个 NUL 截断，空名视为取不到）。
    pub(super) fn face_name(raw: &[u16]) -> Option<String> {
        let end = raw.iter().position(|unit| *unit == 0).unwrap_or(raw.len());
        let name = String::from_utf16_lossy(&raw[..end]).trim().to_string();
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(not(windows))]
mod sys {
    use super::is_installed;

    /// 非 Windows 没有统一的「系统 UI 字体」查询接口：iced 底层把通用无衬线族固定映射到
    /// 几个发行版字体名，这里按常见桌面发行版的默认无衬线族取首个已安装的。
    const CANDIDATES: [&str; 6] =
        ["Open Sans", "Noto Sans", "DejaVu Sans", "Liberation Sans", "Cantarell", "Roboto"];

    pub fn ui_family() -> Option<String> {
        CANDIDATES
            .iter()
            .find(|name| is_installed(name))
            .map(|name| (*name).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn family_entries_merge_aliases_and_dedupe_case_variants() {
        let entries = family_entries(&[
            face(&["ubuntu", "Ubuntu"]),
            face(&["Ubuntu"]),
            face(&["Tahoma"]),
            face(&["Microsoft YaHei", "微软雅黑"]),
            face(&["Microsoft YaHei", "微软雅黑"]),
        ]);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Microsoft YaHei", "Tahoma", "ubuntu"]);
        let yahei = &entries[0];
        assert!(yahei.aliases.iter().any(|alias| alias == "微软雅黑"), "中文别名要能搜到：{yahei:?}");
        assert!(yahei.aliases.iter().any(|alias| alias == "microsoft yahei"));
    }

    #[test]
    fn faces_without_names_are_skipped() {
        assert!(family_entries(&[face(&[]), face(&["Arial"])]).len() == 1);
    }

    /// 索引键一律小写（`family_entries` 落别名时就折叠），大小写无关由 `lookup` 负责。
    #[test]
    fn alias_index_maps_lowercase_aliases_to_display_names() {
        let all = family_entries(&[face(&["Microsoft YaHei", "微软雅黑"]), face(&["Arial"])]);
        let index = alias_index(&all);
        assert_eq!(index.get("microsoft yahei").copied(), Some("Microsoft YaHei"));
        assert_eq!(index.get("微软雅黑").copied(), Some("Microsoft YaHei"));
        assert_eq!(index.get("arial").copied(), Some("Arial"));
        assert!(!index.contains_key("ARIAL"));
        assert!(!index.contains_key("宋体"));
    }

    #[test]
    fn filter_matches_alias_and_ignores_case() {
        let all = family_entries(&[face(&["Microsoft YaHei", "微软雅黑"]), face(&["Arial"])]);
        let hits: Vec<&str> = matching_entries(&all, " 雅黑 ").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(hits, ["Microsoft YaHei"]);
        let hits: Vec<&str> = matching_entries(&all, "ARIAL").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(hits, ["Arial"]);
        assert_eq!(matching_entries(&all, "").len(), 2);
        assert!(matching_entries(&all, "未安装的字").is_empty());
    }

    #[test]
    fn han_display_name_wins_but_keeps_other_records() {
        let names = prefer_display(vec!["SimSun".to_string(), "宋体".to_string()]);
        assert_eq!(names, ["宋体", "SimSun"]);
        // 日文自带名（含假名）不抢展示名
        let names = prefer_display(vec!["Yu Gothic".to_string(), "游ゴシック".to_string()]);
        assert_eq!(names, ["Yu Gothic", "游ゴシック"]);
        let names = prefer_display(vec!["Arial".to_string()]);
        assert_eq!(names, ["Arial"]);
        assert!(is_han_display("微软雅黑"));
        assert!(!is_han_display("Microsoft YaHei"));
    }

    /// 换族必须同时改族名与计数：只改计数会把旧字形留在所有按尺寸/键缓存里，只改族名则缓存不作废。
    #[test]
    fn set_choice_moves_family_and_epoch_together() {
        let mut choice = Choice { family: None, epoch: 0 };
        choice.set(Some("宋体"));
        assert_eq!((choice.family, choice.epoch), (Some("宋体"), 1));
        choice.set(Some("宋体"));
        assert_eq!(choice.epoch, 1, "重复设定同一族不应作废旧度量");
        choice.set(None);
        assert_eq!((choice.family, choice.epoch), (None, 2));
    }

    #[cfg(windows)]
    #[test]
    fn face_name_stops_at_first_nul() {
        let raw: Vec<u16> = "Segoe UI".encode_utf16().chain(std::iter::repeat(0)).take(32).collect();
        assert_eq!(super::sys::face_name(&raw).as_deref(), Some("Segoe UI"));
        let blank = [0u16; 32];
        assert_eq!(super::sys::face_name(&blank), None);
    }
}
