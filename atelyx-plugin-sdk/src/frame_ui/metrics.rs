//! 文字量算缓存：按「文本 + 字号 + 字重 + 字体族 + 可用宽度」缓存宿主的真实字宽。
//! 量算是异步往返：缺项先按估算排版并记下待量算键，补齐后重建这一帧——插件看不到重试。

use std::collections::{HashMap, HashSet};

use atelyx_plugin_protocol::method::{MeasureItem, MeasureTextResult};
use atelyx_plugin_protocol::view::{TextFamily, TextWeight};

use super::model::FontSpec;

type Key = (String, u32, u8, u8, Option<u32>);

/// 缓存条数上限：超出即整体清空（键含文本，长期运行的插件会按历史值累积）。
const MAX_MEASURED: usize = 4096;

#[derive(Default)]
pub(crate) struct Metrics {
    cache: HashMap<Key, MeasureTextResult>,
    misses: Vec<MeasureItem>,
    missed: HashSet<Key>,
}

impl Metrics {
    /// 取量算结果：未命中时按估算值排版并登记待量算。
    pub(crate) fn measure(&mut self, text: &str, spec: FontSpec, max_width: Option<f32>) -> MeasureTextResult {
        let key = key_of(text, spec, max_width);
        if let Some(hit) = self.cache.get(&key) {
            return *hit;
        }
        if self.missed.insert(key.clone()) {
            self.misses.push(MeasureItem {
                text: text.to_string(),
                size: spec.size,
                weight: spec.weight,
                family: spec.family,
                max_width,
            });
        }
        estimate(text, spec.size, max_width)
    }

    /// 单行宽度（无换行）。
    pub(crate) fn width(&mut self, text: &str, spec: FontSpec) -> f32 {
        self.measure(text, spec, None).width
    }

    pub(crate) fn has_misses(&self) -> bool {
        !self.misses.is_empty()
    }

    /// 取走本轮的待量算条目（并按存活条数上限收紧缓存）。
    pub(crate) fn take_misses(&mut self) -> Vec<MeasureItem> {
        self.missed.clear();
        if self.cache.len() > MAX_MEASURED {
            self.cache.clear();
        }
        std::mem::take(&mut self.misses)
    }

    /// 落地一批量算结果：`items` 与 `sizes` 一一对应（宿主可能少回，按较短者取）。
    pub(crate) fn apply(&mut self, items: &[MeasureItem], sizes: &[MeasureTextResult]) {
        for (item, size) in items.iter().zip(sizes) {
            let key = key_of(&item.text, FontSpec { size: item.size, weight: item.weight, family: item.family }, item.max_width);
            self.cache.insert(key, *size);
        }
    }
}

fn key_of(text: &str, spec: FontSpec, max_width: Option<f32>) -> Key {
    (
        text.to_string(),
        spec.size.to_bits(),
        weight_code(spec.weight),
        family_code(spec.family),
        max_width.map(f32::to_bits),
    )
}

fn weight_code(weight: Option<TextWeight>) -> u8 {
    match weight {
        None => 0,
        Some(TextWeight::Normal) => 1,
        Some(TextWeight::Medium) => 2,
        Some(TextWeight::SemiBold) => 3,
    }
}

fn family_code(family: Option<TextFamily>) -> u8 {
    match family {
        None => 0,
        Some(TextFamily::Ui) => 1,
        Some(TextFamily::Mono) => 2,
    }
}

/// 估算：ASCII 按 0.55 字宽，其余按 1 字宽；行高取 1.2 倍字号。
/// 只用于量算落地前的这一两帧，真实字宽一到即被替换。
fn estimate(text: &str, size: f32, max_width: Option<f32>) -> MeasureTextResult {
    let width: f32 = text.chars().map(|c| if c.is_ascii() { 0.55 } else { 1.0 }).sum::<f32>() * size;
    let line_height = size * 1.2;
    match max_width {
        Some(limit) if width > limit => {
            let lines = (width / limit).ceil().max(1.0);
            MeasureTextResult { width: limit, height: line_height * lines, lines: lines as u32 }
        }
        _ => MeasureTextResult { width, height: line_height, lines: 1 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(size: f32) -> FontSpec {
        FontSpec { size, weight: None, family: None }
    }

    #[test]
    fn misses_are_collected_once_per_key_and_replaced_by_measured_values() {
        let mut metrics = Metrics::default();
        metrics.measure("ab", spec(12.0), None);
        metrics.measure("ab", spec(12.0), None);
        assert_eq!(metrics.take_misses().len(), 1, "同一键只量算一次");

        let items = vec![MeasureItem { text: "ab".into(), size: 12.0, weight: None, family: None, max_width: None }];
        metrics.apply(&items, &[MeasureTextResult { width: 13.0, height: 14.0, lines: 1 }]);
        let measured = metrics.measure("ab", spec(12.0), None);
        assert_eq!(measured.width, 13.0, "量算落地后取真实字宽");
        assert!(!metrics.has_misses());
    }

    #[test]
    fn estimate_uses_a_wider_char_for_cjk() {
        let latin = estimate("aaaa", 10.0, None);
        let cjk = estimate("中文", 10.0, None);
        assert_eq!(latin.width, 22.0);
        assert_eq!(cjk.width, 20.0);
        assert!(cjk.height > 0.0);
    }
}
