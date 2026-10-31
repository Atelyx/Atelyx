#!/usr/bin/env bash
# 构建同进程 ABI 的 sample 插件与宿主，并跑冒烟 / 延迟基准。
# 用法：bash scripts/plugin-abi-sample.sh [release]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profile="${1:-debug}"
case "$profile" in
  debug)
    cargo build -p atelyx-plugin-sample -p atelyx-plugin-host
    out="target/debug"
    ;;
  release)
    cargo build --release -p atelyx-plugin-sample -p atelyx-plugin-host
    out="target/release"
    ;;
  *)
    echo "未知构建档位：$profile（只接受 debug / release）" >&2
    exit 1
    ;;
esac

case "$(uname -s)" in
  Linux*) host="$out/sample-host" ;;
  MINGW*|MSYS*|CYGWIN*) host="$out/sample-host.exe" ;;
  *) echo "未支持的平台：$(uname -s)" >&2; exit 1 ;;
esac

"$host"
