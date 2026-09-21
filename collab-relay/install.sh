#!/usr/bin/env bash
# Atelyx 协作服务端一键安装（Linux + systemd 宿主机直跑，不用 Docker）。
#
# 用法（在仓库克隆目录内）：
#   sudo bash collab-relay/install.sh            # 默认端口 11224，数据目录 /var/lib/atelyx
#   sudo bash collab-relay/install.sh 13000      # 自定义端口
#   sudo bash collab-relay/install.sh 11224 /mnt/nas/atelyx-data   # 自定义数据目录（如 NAS 挂载点）
#
# 重复执行 = 更新二进制并重启服务，数据不受影响。
# 可选 TLS：安装后编辑 /etc/systemd/system/atelyx-server.service 加两行
#   Environment=TLS_CERT=/path/cert.pem
#   Environment=TLS_KEY=/path/key.pem
# 再 systemctl daemon-reload && systemctl restart atelyx-server。
set -euo pipefail

PORT="${1:-11224}"
DATA_DIR="${2:-/var/lib/atelyx}"
SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_SRC="$SRC_DIR/target/release/collab-relay"
INSTALL_DIR="/opt/atelyx-server"
SERVICE=/etc/systemd/system/atelyx-server.service
SERVICE_USER="atelyx"

[ "$(id -u)" = 0 ] || { echo "请用 sudo 运行"; exit 1; }

if [ ! -x "$BIN_SRC" ]; then
    if command -v cargo >/dev/null 2>&1; then
        echo "未找到已构建产物，开始构建（首次约数分钟）…"
        (cd "$SRC_DIR" && cargo build --release)
    else
        echo "未找到 target/release/collab-relay，且本机没有 cargo。"
        echo "先安装 Rust（https://rustup.rs）后重试，或在有 Rust 的机器上构建后把 target/release/collab-relay 拷到 $SRC_DIR/target/release/"
        exit 1
    fi
fi

# 专用系统用户（无登录 shell）：服务不跑在 root 下
id -u "$SERVICE_USER" >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"

install -Dm755 "$BIN_SRC" "$INSTALL_DIR/collab-relay"
install -d -m750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"

cat > "$SERVICE" <<EOF
[Unit]
Description=Atelyx 协作服务端
After=network.target

[Service]
User=$SERVICE_USER
ExecStart=$INSTALL_DIR/collab-relay
Environment=PORT=$PORT
Environment=DATA_DIR=$DATA_DIR
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable atelyx-server
systemctl restart atelyx-server

echo "----------------------------------------"
echo "Atelyx 协作服务端已启动"
echo "  端口：$PORT"
echo "  数据目录：$DATA_DIR"
echo "  服务管理：systemctl status/restart/stop atelyx-server"
echo "  建空间时如需收编已有仓库文件夹，path 传该文件夹的绝对路径"
echo "----------------------------------------"
