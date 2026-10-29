#!/usr/bin/env bash
# 构建随包预置插件，按插件 id 铺到目标目录：<目标>/<id>/{plugin.json, 可执行文件}。
# 用法：bash scripts/build-plugins.sh <目标目录> [debug|release]（缺省 release）
# 开发期铺到 target/debug/preset-plugins；打包期铺到 target/preset-plugins 再进入安装包。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:?用法：build-plugins.sh <目标目录> [debug|release]}"
profile="${2:-release}"
cd "$repo_root"

# 预置插件清单（crate 名）；落点目录取各自 plugin.json 的 id
crates=(atelyx-plugin-calendar)

cargo_cmd=(cargo build)
if [ "$profile" = "release" ]; then cargo_cmd+=(--release); fi
for crate in "${crates[@]}"; do cargo_cmd+=(-p "$crate"); done
"${cargo_cmd[@]}"

mkdir -p "$dest"
for crate in "${crates[@]}"; do
  id="$(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$crate/plugin.json" | head -n1)"
  [ -n "$id" ] || { echo "错误：$crate/plugin.json 未读出 id" >&2; exit 1; }
  if [ "$profile" = "release" ]; then bin_dir="target/release"; else bin_dir="target/debug"; fi
  if [ -f "$bin_dir/$crate.exe" ]; then bin="$bin_dir/$crate.exe"; else bin="$bin_dir/$crate"; fi
  [ -f "$bin" ] || { echo "错误：未找到构建产物 $bin" >&2; exit 1; }
  out="$dest/$id"
  rm -rf "$out"
  mkdir -p "$out"
  cp "$crate/plugin.json" "$out/"
  install -m755 "$bin" "$out/$(basename "$bin")"
  echo "==> 预置插件：$out"
done
