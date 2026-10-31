#!/usr/bin/env bash
# 构建随包预置插件，按插件 id 铺到目标目录：<目标>/<id>/{plugin.json, 当前平台入口}。
# 预置插件都在 plugins/<名字>/（带 plugin.json），脚本按目录发现，不维护 crate 清单。
# 用法：bash scripts/build-plugins.sh <目标目录> [debug|release]（缺省 release）
# 开发期铺到 target/debug/preset-plugins；打包期铺到 target/preset-plugins 再进入安装包。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:?用法：build-plugins.sh <目标目录> [debug|release]}"
profile="${2:-release}"
cd "$repo_root"

shopt -s nullglob
manifests=(plugins/*/plugin.json)
shopt -u nullglob
[ ${#manifests[@]} -gt 0 ] || { echo "错误：plugins/ 下没有插件清单" >&2; exit 1; }

cargo_cmd=(cargo build)
if [ "$profile" = "release" ]; then cargo_cmd+=(--release); fi
for manifest in "${manifests[@]}"; do
  dir="$(dirname "$manifest")"
  name="$(sed -n 's/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -n1)"
  [ -n "$name" ] || { echo "错误：$dir/Cargo.toml 未读出包名" >&2; exit 1; }
  cargo_cmd+=(-p "$name")
done
"${cargo_cmd[@]}"

mkdir -p "$dest"
for manifest in "${manifests[@]}"; do
  id="$(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n1)"
  [ -n "$id" ] || { echo "错误：$manifest 未读出 id" >&2; exit 1; }
  if [ "$profile" = "release" ]; then bin_dir="target/release"; else bin_dir="target/debug"; fi
  case "$(uname -s)" in
    Linux*) entry="$(sed -n 's/.*"linux"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n1)" ;;
    MINGW*|MSYS*|CYGWIN*) entry="$(sed -n 's/.*"windows"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n1)" ;;
    *) echo "未支持的平台：$(uname -s)" >&2; exit 1 ;;
  esac
  [ -n "$entry" ] || { echo "错误：$manifest 未声明当前平台入口" >&2; exit 1; }
  bin="$bin_dir/$entry"
  [ -f "$bin" ] || { echo "错误：未找到构建产物 $bin（入口名须与 crate 产物一致）" >&2; exit 1; }
  out="$dest/$id"
  rm -rf "$out"
  mkdir -p "$out"
  cp "$manifest" "$out/"
  install -m755 "$bin" "$out/$(basename "$bin")"
  echo "==> 预置插件：$out"
done
