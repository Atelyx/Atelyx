# Vendor patches（补丁文件形态）

工作区 `[patch.crates-io]` 指向本目录下的 crate 源码目录（`winit/`、`iced_widget/`）。
**仓库只保存补丁文件本身**；源码目录是物化产物（gitignore），新克隆后、首次构建前执行一次：

```sh
bash patches/apply.sh
```

脚本优先复用本机 cargo 缓存（`$CARGO_HOME/registry/src`），否则从 crates.io 下载精确版本的
原始源码，然后 `git apply` 对应的 `*.patch`。物化目录删除后重跑脚本即可重建。

## 补丁清单

### winit-0.30.13.patch

基线：winit 0.30.13（crates.io）。改动仅在 `src/platform_impl/windows/`：

- `event_loop.rs`：`WM_DPICHANGED` 的自算窗口重定位（光标比例偏置、显示器微推循环）
  只在 Windows 10（build < 22000）执行；Windows 11+ 直接采用系统建议矩形。
  Win11 上自算重定位导致跨 DPI 显示器拖动时窗口跳变 / 振荡 / 尺寸异常。
  与上游 rust-windowing/winit#4341 同改（已进上游，未随任何 0.30.x 发布）。
- `dark_mode.rs`：`WIN10_BUILD_VERSION` 改 `pub(crate)`（供 `event_loop.rs` 使用）。

iced 依赖栈拉到含上游 #4341 的 winit 发布版后，删除本补丁、物化目录与根 `Cargo.toml`
的 `[patch.crates-io]` 条目。

### iced_widget-0.14.2.patch

基线：iced_widget 0.14.2（crates.io）。改动 `src/keyed/column.rs` 与文本输入件：

- `keyed_column` 按 key 复用子树时直接调 `Widget::diff`、不校验部件 tag，且「中间插入」
  会走错分支把子树数组与 key 数组错位对齐——同 key 控件种类在整树重发间变化即
  downcast panic。修法：复用回调改走 `Tree::diff`（tag 不同整树重建），与上游
  iced-rs/iced#3488 的修法一致（未随任何 0.14.x 发布）。
  对账语义回归测试：`atelyx-app/tests/keyed_column_diff.rs`。
- `text_input.rs` / `text_editor.rs`：IME 组合中的回车是确认候选（`KeyPressed` 先于
  `Ime::Commit` 送达），原样触发 `on_submit` / 键位绑定会把未上屏的组合串当一次提交。
  组合期（preedit 挂起）的 Enter 不进提交路径，文本由随后的 `Ime::Commit` 上屏。
- `text_input.rs` / `text_editor.rs`：空组合串按「组合已取消」处理（清成 `None`）。winit
  以 `Ime::Preedit("")` 表示组合结束，fcitx5 等输入法在静止态也会持续重发；原样存成
  空 preedit 会让上面那处回车判定与整段按键处理把常态误判为组合中（回车 / 退格全失效，
  文字仍能经 `Ime::Commit` 进入）。

iced 升级到含 #3488 修复的版本后，删除本补丁、物化目录与根 `Cargo.toml` 的
`[patch.crates-io]` 条目。

## 新增 / 修改补丁的流程

1. 物化目录里直接改代码（或从新的原始源码改起）；
2. 用「原始源码 vs 物化目录」的差异生成补丁（只含真正改动的文件）：

   ```sh
   diff -ruN <原始源码>/<file> <物化目录>/<file>   # 头部改写成 a/ b/ 前缀
   ```

3. 更新本清单的基线版本与改动说明。
