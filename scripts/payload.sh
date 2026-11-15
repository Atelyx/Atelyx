#!/usr/bin/env bash
# 各 Linux 产物共用的 usr/ 数据清单；调用方须先设置 repo_root。

stage_payload() {
  local stage="$1" exe="$2"
  local icons="$repo_root/assets/icons"
  local notices="$repo_root/docs/THIRD_PARTY_NOTICES.md"
  install -Dm755 "$exe" "$stage/usr/bin/atelyx"
  install -Dm644 "$repo_root/packaging/linux/com.atelyx.desktop" "$stage/usr/share/applications/com.atelyx.desktop"
  install -Dm644 "$repo_root/LICENSE" "$stage/usr/share/licenses/atelyx/LICENSE"
  install -Dm644 "$notices" "$stage/usr/share/licenses/atelyx/THIRD_PARTY_NOTICES.md"
  install -Dm644 "$icons/32x32.png" "$stage/usr/share/icons/hicolor/32x32/apps/atelyx.png"
  install -Dm644 "$icons/64x64.png" "$stage/usr/share/icons/hicolor/64x64/apps/atelyx.png"
  install -Dm644 "$icons/128x128.png" "$stage/usr/share/icons/hicolor/128x128/apps/atelyx.png"
  install -Dm644 "$icons/128x128@2x.png" "$stage/usr/share/icons/hicolor/256x256/apps/atelyx.png"
}
