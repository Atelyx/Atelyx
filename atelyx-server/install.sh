#!/usr/bin/env bash
# Atelyx 协作服务端一键安装/更新（Linux + systemd 宿主机直跑，不用 Docker）。
#
# 用法（在仓库克隆目录内）：
#   sudo bash atelyx-server/install.sh            # 默认端口 11224，数据目录 /var/lib/atelyx
#   sudo bash atelyx-server/install.sh 13000      # 自定义端口
#   sudo bash atelyx-server/install.sh 11224 /mnt/nas/atelyx-data   # 自定义数据目录（如 NAS 挂载点）
#
# 每次执行完成完整更新：拉取最新代码 → 增量编译 → 安装并重启服务，数据不受影响。
# 不带参数重复执行时沿用上次安装的端口/数据目录（记录于 /opt/atelyx-server/install.conf），
# 显式传参仍可覆盖。仓库内有未提交的本地改动时拒绝拉取并退出，避免覆盖服务器上的手工修改。
# 可选 TLS：安装后编辑 /etc/systemd/system/atelyx-server.service 加两行
#   Environment=TLS_CERT=/path/cert.pem
#   Environment=TLS_KEY=/path/key.pem
# 再 systemctl daemon-reload && systemctl restart atelyx-server；
# 之后的更新重装会自动保留已写入的 TLS 环境行。
set -euo pipefail

INSTALL_DIR="/opt/atelyx-server"
SERVICE=/etc/systemd/system/atelyx-server.service
SERVICE_USER="atelyx"
CONF_FILE="$INSTALL_DIR/install.conf"
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(dirname "$SRC_DIR")"
BIN_SRC="$SRC_DIR/target/release/atelyx-server"

[ "$(id -u)" = 0 ] || { echo "请用 sudo 运行"; exit 1; }

# 上次安装记录的端口/数据目录：无参数更新时沿用，避免悄悄回退到默认值
RECORDED_PORT=""
RECORDED_DATA_DIR=""
if [ -f "$CONF_FILE" ]; then
    # shellcheck disable=SC1090
    . "$CONF_FILE"
fi
PORT="${1:-${RECORDED_PORT:-11224}}"
DATA_DIR="${2:-${RECORDED_DATA_DIR:-/var/lib/atelyx}}"

# sudo 会重置环境变量，cargo 可能装在调用方用户而非 root 的家目录下
find_cargo() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi
    local env_file user_home
    for env_file in "$HOME/.cargo/env" \
        "$(getent passwd "${SUDO_USER:-}" 2>/dev/null | cut -d: -f6)/.cargo/env"; do
        [ -f "$env_file" ] || continue
        user_home="$(dirname "$(dirname "$env_file")")"
        # shellcheck disable=SC1090
        . "$env_file"
        # rustup 垫片按 RUSTUP_HOME 定位工具链，默认取当前用户家目录，跨用户调用须显式指向
        export CARGO_HOME="$user_home/.cargo" RUSTUP_HOME="$user_home/.rustup"
        if command -v cargo >/dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

# .git 在 worktree/子模块检出下是文件而非目录，须用 -e 判断，否则会静默跳过拉取
if [ -e "$REPO_DIR/.git" ]; then
    if [ -n "$(git -C "$REPO_DIR" status --porcelain --untracked-files=no)" ]; then
        echo "仓库 $REPO_DIR 有未提交的本地改动，为避免覆盖请先处理后重试："
        git -C "$REPO_DIR" status --short
        exit 1
    fi
    echo "拉取最新代码…"
    git -C "$REPO_DIR" pull --ff-only
fi

if find_cargo; then
    echo "编译（增量构建，通常很快）…"
    (cd "$SRC_DIR" && cargo build --release)
elif [ -x "$BIN_SRC" ]; then
    echo "警告：本机没有可用 cargo，跳过编译，安装已有产物 $BIN_SRC（可能不含最新代码）。"
else
    echo "未找到 target/release/atelyx-server，且本机没有 cargo。"
    echo "先安装 Rust（https://rustup.rs）后重试，或在有 Rust 的机器上构建后把 target/release/atelyx-server 拷到 $SRC_DIR/target/release/"
    exit 1
fi

# 专用系统用户（无登录 shell）：服务不跑在 root 下
id -u "$SERVICE_USER" >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"

install -Dm755 "$BIN_SRC" "$INSTALL_DIR/atelyx-server"
install -d -m750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"

# 服务文件被整体重写，先取出手工加入的 TLS 环境行写回新文件，避免更新时丢失
TLS_ENV_BLOCK=""
if [ -f "$SERVICE" ]; then
    tls_lines="$(grep -E '^Environment=TLS_(CERT|KEY)=' "$SERVICE" || true)"
    [ -z "$tls_lines" ] || TLS_ENV_BLOCK="$tls_lines"$'\n'
fi

cat > "$SERVICE" <<EOF
[Unit]
Description=Atelyx 协作服务端
After=network.target

[Service]
User=$SERVICE_USER
ExecStart=$INSTALL_DIR/atelyx-server
Environment=PORT=$PORT
Environment=DATA_DIR=$DATA_DIR
${TLS_ENV_BLOCK}Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable atelyx-server
systemctl restart atelyx-server

printf 'RECORDED_PORT=%q\nRECORDED_DATA_DIR=%q\n' "$PORT" "$DATA_DIR" > "$CONF_FILE"

echo "----------------------------------------"
echo "Atelyx 协作服务端已启动"
echo "  端口：$PORT"
echo "  数据目录：$DATA_DIR"
echo "  服务管理：systemctl status/restart/stop atelyx-server"
echo "  建空间时如需收编已有仓库文件夹，path 传该文件夹的绝对路径"
echo "----------------------------------------"
