#!/usr/bin/env bash
# 构建 pacman 包并产出本地仓库到 target/dist/pacman/x86_64/。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
export CARGO_TARGET_DIR="$repo_root/target"
. "$repo_root/scripts/payload.sh"

version="$(sed -n '/^\[package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' apps/atelyx/Cargo.toml | head -n1)"
if [ -z "$version" ]; then
  echo "错误：未能从 apps/atelyx/Cargo.toml 读到版本号" >&2
  exit 1
fi

exe="$repo_root/target/release/atelyx"
if [ ! -f "$exe" ]; then
  echo "==> cargo build -p atelyx --release（版本 $version）"
  cargo build -p atelyx --release
fi

# 工作目录放 /tmp：项目挂载（NTFS）不保留权限位，打包会把挂载默认权限带进包
work="$(mktemp -d "${TMPDIR:-/tmp}/atelyx-pkg.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/atelyx"
stage_payload "$work/atelyx" "$exe"

cat > "$work/atelyx/PKGBUILD" <<EOF
pkgname=atelyx
pkgver=$version
pkgrel=1
pkgdesc="以人为本的可拓展桌面工作台"
arch=(x86_64)
url="https://github.com/Atelyx/Atelyx"
license=(Apache)
options=(strip !debug)
depends=(wayland libxkbcommon libx11 fontconfig freetype2 vulkan-icd-loader hicolor-icon-theme)
package() {
  cp -a "\$startdir/usr" "\$pkgdir/usr"
}
EOF

echo "==> makepkg 构建 pacman 包"
(cd "$work/atelyx" && makepkg --force)

pkgfile="$(cd "$work/atelyx" && ls atelyx-"$version"-*-x86_64.pkg.tar.* | head -n1)"
repo_dir="$repo_root/target/dist/pacman/x86_64"
mkdir -p "$repo_dir"
cp "$work/atelyx/$pkgfile" "$repo_dir/"
(cd "$repo_dir" && repo-add atelyx.db.tar.zst "$pkgfile")

# 设置 ATELYX_REPO_GPG_KEY 出分离签名：签名后接入方用默认 SigLevel，未签名须显式降级 Optional
if [ -n "${ATELYX_REPO_GPG_KEY:-}" ]; then
  echo "==> GPG 签名（$ATELYX_REPO_GPG_KEY）"
  (cd "$repo_dir" && for f in "$pkgfile" atelyx.db.tar.zst atelyx.files.tar.zst; do
    gpg --batch --yes --detach-sign --use-agent --local-user "$ATELYX_REPO_GPG_KEY" "$f"
  done)
fi

echo "==> 仓库：$repo_dir"
if [ -n "${ATELYX_REPO_GPG_KEY:-}" ]; then
  echo "==> 对外发布：把 x86_64/ 整目录（含 .sig）放到静态托管，公布密钥指纹 $ATELYX_REPO_GPG_KEY"
else
  echo "==> 接入：在 /etc/pacman.conf 追加以下条目后 sudo pacman -Syu atelyx"
  cat <<EOF

[atelyx]
SigLevel = Optional
Server = file://$repo_dir
EOF
fi
