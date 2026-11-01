#!/usr/bin/env bash
# 构建同进程 ABI 的 sample 插件与参考宿主示例，并跑冒烟 / 延迟基准。
# 用法：bash scripts/plugin-abi-sample.sh [release]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profile="${1:-debug}"
case "$profile" in
  debug)
    cargo build -p atelyx-plugin-sample
    cargo run -p atelyx-core --example abi_sample_host
    ;;
  release)
    cargo build --release -p atelyx-plugin-sample
    cargo run --release -p atelyx-core --example abi_sample_host
    ;;
  *)
    echo "未知构建档位：$profile（只接受 debug / release）" >&2
    exit 1
    ;;
esac
