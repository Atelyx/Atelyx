#!/usr/bin/env bash
# 物化被补丁的 crate：Cargo 的 [patch.crates-io] 只接受完整源码目录，仓库里保存的是
# 补丁文件本身（*.patch），本脚本从 crates.io 原始源码生成目录并应用补丁。
# 新克隆后、首次构建前跑一次即可：bash patches/apply.sh
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "$(cd "$(dirname "$0")/.." && pwd)")"
cd "$ROOT"

REGISTRY_SRC="${CARGO_HOME:-$HOME/.cargo}/registry/src"

materialize() {
  local name=$1 version=$2
  local dir="patches/$name"
  # 完成判据是标记文件：cp 成功但 apply 失败的半成品目录会在重跑时推倒重来
  if [ -f "$dir/.patched" ]; then
    echo "[$name-$version] 已存在，跳过（如需重建：删除 $dir 后重跑）"
    return
  fi
  rm -rf "$dir"
  local patch_file="patches/$name-$version.patch"
  local cache
  cache=$(ls -d "$REGISTRY_SRC"/*/"$name-$version" 2>/dev/null | head -1 || true)
  if [ -n "$cache" ]; then
    echo "[$name-$version] 复用本机 cargo 缓存：$cache"
    cp -r "$cache" "$dir"
  else
    echo "[$name-$version] 从 crates.io 下载原始源码"
    curl -fsSL "https://static.crates.io/crates/$name/$name-$version.crate" | tar xz
    rm -rf "$dir"
    mv "$name-$version" "$dir"
  fi
  # Linux 上 cargo 缓存文件是只读的，git apply 需要可写
  chmod -R u+w "$dir"
  # git apply 在仓库内按仓库根解析路径：--directory 把补丁定位到本 crate 的物化目录
  git apply --directory="$dir" --whitespace=nowarn "$patch_file"
  touch "$dir/.patched"
  echo "[$name-$version] 已应用补丁 patches/$name-$version.patch"
}

# 补丁清单：crate 名 + 精确版本（与 *.patch 文件一一对应）
materialize winit 0.30.13
materialize iced_widget 0.14.2
