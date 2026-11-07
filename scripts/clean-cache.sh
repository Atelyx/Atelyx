#!/usr/bin/env bash
# 构建缓存瘦身：清空 Cargo 永不自动回收的增量会话目录（target/*/debug/incremental）。
# 用法：bash scripts/clean-cache.sh
# 代价：下次构建需重编本仓库 crate（依赖 rlib 保留）；不影响源码与 target/dist 产物。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

size_mb() {
  [ -d "$1" ] || { echo 0; return; }
  du -sm "$1" 2>/dev/null | cut -f1
}

before=0
after=0
# CARGO_TARGET_DIR 指向仓库外时（MSVC 非 ASCII 路径约束），真正的增量缓存也在那里
targets=("$repo_root/target" "$repo_root/apps/server/target")
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  targets+=("$CARGO_TARGET_DIR")
fi
for t in "${targets[@]}"; do
  [ -d "$t" ] || continue
  before=$((before + $(size_mb "$t")))
  while IFS= read -r -d '' dir; do
    echo "==> 清空 $dir"
    rm -rf "${dir:?}"/*
  done < <(find "$t" -type d -path '*/debug/incremental' -print0)
  after=$((after + $(size_mb "$t")))
done

echo "==> 释放 $((before - after)) MB（剩余缓存 $after MB）"
