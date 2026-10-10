//! 布局模型层整体再导出：实现在 `atelyx-core::layout`，两壳共用同一模型与磁盘格式。
//! 壳内代码统一经 `crate::layout_model::` 引用，模型变更只发生在 core 一处。

pub use atelyx_core::layout::*;
