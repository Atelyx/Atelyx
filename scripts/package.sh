#!/usr/bin/env bash
# 打包入口：构建 release 产物并产出当前平台的安装包到 target/dist/。
#   Windows：Atelyx_<版本>_x64-setup.exe（NSIS，命名与更新链路约定一致）
#   Linux：  atelyx_<版本>_amd64.deb 与 Atelyx_<版本>-x86_64.AppImage
# 各平台须在对应系统上分别打包。makensis 依次查找：$NSIS_EXE、PATH、NSIS 标准安装路径。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
dist="$repo_root/target/dist"
notices="$repo_root/docs/THIRD_PARTY_NOTICES.md"
icon="$repo_root/atelyx-app/assets/icons/icon.ico"
nsi="$repo_root/distribution/nsis/installer.nsi"

# 版本号唯一来源：atelyx-app 的 [package] version
version="$(sed -n '/^\[package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' atelyx-app/Cargo.toml | head -n1)"
if [ -z "$version" ]; then
  echo "错误：未能从 atelyx-app/Cargo.toml 读到版本号" >&2
  exit 1
fi

# 转 Windows 反斜杠路径（NSIS 的 File 指令不识别正斜杠）；非 Git Bash 环境原样返回
win_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

find_makensis() {
  if [ -n "${NSIS_EXE:-}" ]; then printf '%s' "$NSIS_EXE"; return 0; fi
  if command -v makensis >/dev/null 2>&1; then command -v makensis; return 0; fi
  local dir
  for dir in "/c/Program Files (x86)/NSIS" "/c/Program Files/NSIS"; do
    if [ -x "$dir/makensis.exe" ]; then printf '%s' "$dir/makensis.exe"; return 0; fi
  done
  return 1
}

package_windows() {
  local exe="$repo_root/target/release/atelyx.exe"
  [ -f "$exe" ] || { echo "错误：未找到 $exe" >&2; exit 1; }
  local makensis
  makensis="$(find_makensis)" || {
    echo "错误：未找到 makensis；安装 NSIS 后重试，或用 NSIS_EXE 环境变量指向 makensis.exe" >&2
    exit 1
  }
  local log="$dist/nsis-build.log"
  echo "==> makensis：产出 Windows 安装包"
  # 编译器输出整份落日志：失败必须把尾部回显到 stderr，不得只留一个非零退出码
  if ! "$makensis" \
    -DVERSION="$version" \
    -DEXE="$(win_path "$exe")" \
    -DLICENSE="$(win_path "$repo_root/LICENSE")" \
    -DNOTICES="$(win_path "$notices")" \
    -DICON="$(win_path "$icon")" \
    -DOUTFILE="$(win_path "$dist/Atelyx_${version}_x64-setup.exe")" \
    "$(win_path "$nsi")" >"$log"; then
    echo "错误：makensis 编译失败，日志尾部：" >&2
    tail -n 20 "$log" >&2
    exit 1
  fi
  echo "==> 产物：$dist/Atelyx_${version}_x64-setup.exe"
}

# 组装 usr/ 数据布局（deb 与 AppImage 共用同一套文件清单）
stage_payload() {
  local stage="$1" exe="$2"
  local icons="$repo_root/atelyx-app/assets/icons"
  install -Dm755 "$exe" "$stage/usr/bin/atelyx"
  install -Dm644 "$repo_root/distribution/linux/com.atelyx.desktop" "$stage/usr/share/applications/com.atelyx.desktop"
  install -Dm644 "$repo_root/LICENSE" "$stage/usr/share/licenses/atelyx/LICENSE"
  install -Dm644 "$notices" "$stage/usr/share/licenses/atelyx/THIRD_PARTY_NOTICES.md"
  install -Dm644 "$icons/32x32.png" "$stage/usr/share/icons/hicolor/32x32/apps/atelyx.png"
  install -Dm644 "$icons/64x64.png" "$stage/usr/share/icons/hicolor/64x64/apps/atelyx.png"
  install -Dm644 "$icons/128x128.png" "$stage/usr/share/icons/hicolor/128x128/apps/atelyx.png"
  install -Dm644 "$icons/128x128@2x.png" "$stage/usr/share/icons/hicolor/256x256/apps/atelyx.png"
}

package_deb() {
  local exe="$1"
  local stage="$repo_root/target/pkg/deb"
  rm -rf "$stage"
  stage_payload "$stage/data" "$exe"
  mkdir -p "$stage/DEBIAN"
  cat > "$stage/DEBIAN/control" <<EOF
Package: atelyx
Version: $version
Architecture: amd64
Maintainer: Atelyx <atelyx@users.noreply.github.com>
Depends: libx11-6, libwayland-client0, libxkbcommon0, libfontconfig1, libfreetype6
Section: utils
Priority: optional
Description: 以人为本的可拓展桌面工作台
 对话、笔记、表格、文件在同一工作台，自建服务端即可开启多人实时协作。
EOF
  # 包内文件属主归 root，避免按构建用户落盘
  (cd "$stage/data" && find . -type f -print0 | tar --null -T - --owner=0 --group=0 -czf "$stage/data.tar.gz")
  (cd "$stage/DEBIAN" && tar --owner=0 --group=0 -czf "$stage/control.tar.gz" .)
  printf '2.0\n' > "$stage/debian-binary"
  # ar 对已存在归档只替换成员、保留原序：先删旧包，保证 debian-binary 恒为首个成员
  rm -f "$dist/atelyx_${version}_amd64.deb"
  ar rcs "$dist/atelyx_${version}_amd64.deb" \
    "$stage/debian-binary" "$stage/control.tar.gz" "$stage/data.tar.gz"
  echo "==> 产物：$dist/atelyx_${version}_amd64.deb"
}

package_appimage() {
  local exe="$1"
  local appdir="$repo_root/target/pkg/appimage/Atelyx.AppDir"
  rm -rf "$appdir"
  mkdir -p "$appdir"
  # stage_payload 自带 usr/ 前缀，这里传 AppDir 根，否则会铺成 usr/usr/
  stage_payload "$appdir" "$exe"
  # AppImage 惯例：AppRun、desktop 与图标置于 AppDir 根
  ln -s usr/bin/atelyx "$appdir/AppRun"
  cp "$repo_root/distribution/linux/com.atelyx.desktop" "$appdir/atelyx.desktop"
  cp "$repo_root/atelyx-app/assets/icons/128x128.png" "$appdir/atelyx.png"
  ln -sf atelyx.png "$appdir/.DirIcon"
  if ! command -v appimagetool >/dev/null 2>&1; then
    echo "错误：未找到 appimagetool；从 https://github.com/AppImage/appimagetool/releases 下载后放入 PATH" >&2
    exit 1
  fi
  local log="$dist/appimage-build.log"
  # 同上：appimagetool 失败要把日志尾部回显出来（缺 mksquashfs 等依赖只在日志里可见）
  if ! ARCH=x86_64 appimagetool "$appdir" "$dist/Atelyx_${version}-x86_64.AppImage" >"$log"; then
    echo "错误：appimagetool 打包失败，日志尾部：" >&2
    tail -n 20 "$log" >&2
    exit 1
  fi
  echo "==> 产物：$dist/Atelyx_${version}-x86_64.AppImage"
}

package_linux() {
  local exe="$repo_root/target/release/atelyx"
  [ -f "$exe" ] || { echo "错误：未找到 $exe" >&2; exit 1; }
  package_deb "$exe"
  package_appimage "$exe"
}

mkdir -p "$dist"
echo "==> cargo build -p atelyx-app --release（版本 $version）"
cargo build -p atelyx-app --release

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) package_windows ;;
  Linux) package_linux ;;
  *)
    echo "错误：不支持的平台（$(uname -s)）；Windows 与 Linux 须分别在对应系统上打包" >&2
    exit 1
    ;;
esac
