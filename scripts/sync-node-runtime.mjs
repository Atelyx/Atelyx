/**
 * 把随应用分发的 Node 运行时同步到 `src-tauri/resources/runtime/`。
 *
 * 为什么需要它：插件经 `ctx.process` 起宿主捆绑的脚本运行时获得完整 JS 能力（用户机器不预装
 * Node）。二进制体积几十 MB，不入库（见 .gitignore），构建前从 nodejs.org 官方 dist 下载锁定
 * 版本——因此构建机需要联网（首次；之后按版本戳跳过）。
 *
 * 目标目录始终存在（.gitkeep 入库）：`bundle.resources` 指向目录，缺少二进制时
 * `ctx.process.bundledRuntime()` 返回 null（插件按未分发降级），而不是让 `cargo build` 直接失败。
 *
 * 只同步当前构建平台的二进制：`bundle.resources` 整目录打包，另一平台的同名二进制会被清掉，
 * 避免安装包白背一份跨平台载荷。
 *
 * 升级运行时：改下方 NODE_VERSION（取 https://nodejs.org 的 LTS 线最新版）。
 *
 * 入口：挂在 `pnpm run tauri:dev` / `tauri:build` 前置；直接用 `pnpm tauri build` 会绕过同步。
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/** 锁定的 Node 版本（LTS 线；升版只改这里）。 */
const NODE_VERSION = "24.21.0";
const DIST_BASE = `https://nodejs.org/dist/v${NODE_VERSION}`;

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const destDir = path.join(root, "src-tauri", "resources", "runtime");

/** 当前构建平台的分发包形态：归档地址 + 包内二进制与许可文件的成员路径。 */
function targetFor(platform) {
  if (platform === "win32") {
    const stem = `node-v${NODE_VERSION}-win-x64`;
    return {
      archive: `${DIST_BASE}/${stem}.zip`,
      sumKey: `${stem}.zip`,
      members: {
        binary: { from: `${stem}/node.exe`, to: "node.exe" },
        license: { from: `${stem}/LICENSE`, to: "LICENSE" },
      },
      staleBinaries: ["node"],
    };
  }
  if (platform === "linux") {
    const stem = `node-v${NODE_VERSION}-linux-x64`;
    return {
      archive: `${DIST_BASE}/${stem}.tar.gz`,
      sumKey: `${stem}.tar.gz`,
      members: {
        binary: { from: `${stem}/bin/node`, to: "node" },
        license: { from: `${stem}/LICENSE`, to: "LICENSE" },
      },
      staleBinaries: ["node.exe"],
    };
  }
  return null;
}

/** 失败以异常抛出（不用 process.exit：直接退出会跳过 finally，把临时目录留在资源里）。 */
function fail(message) {
  throw new Error(`[node-runtime:sync] ${message}`);
}

const target = targetFor(process.platform);
if (!target) {
  // 移动端不分发运行时（插件侧可探测降级），同步直接成功跳过。
  console.log(`[node-runtime:sync] 平台 ${process.platform} 不分发运行时，跳过`);
  process.exit(0);
}

const binaryDest = path.join(destDir, target.members.binary.to);
const licenseDest = path.join(destDir, target.members.license.to);
const versionJsonDest = path.join(destDir, "version.json");
const stampDest = path.join(destDir, ".version");
const stampText = `${NODE_VERSION} ${target.members.binary.to}`;

function isUpToDate() {
  try {
    return (
      fs.existsSync(licenseDest) &&
      fs.existsSync(versionJsonDest) &&
      fs.statSync(binaryDest).size > 0 &&
      fs.readFileSync(stampDest, "utf8") === stampText
    );
  } catch {
    return false;
  }
}

if (isUpToDate()) {
  console.log(`[node-runtime:sync] 已是最新（Node ${NODE_VERSION}）`);
  process.exit(0);
}

async function download(url, dest) {
  // 超时兜底：官方 dist 偶发挂起不能让构建无限等待。
  const res = await fetch(url, { signal: AbortSignal.timeout(120_000) });
  if (!res.ok) fail(`下载失败（HTTP ${res.status}）：${url}`);
  const buf = Buffer.from(await res.arrayBuffer());
  fs.writeFileSync(dest, buf);
  return buf;
}

/** 归档 sha256 必须与官方 SHASUMS256.txt 对上：传输损坏/截断在这里拦下，不带病打包。 */
async function downloadAndVerify(url, sumKey, dest) {
  const buf = await download(url, dest);
  const actual = createHash("sha256").update(buf).digest("hex");
  let sums;
  try {
    sums = await (await fetch(`${DIST_BASE}/SHASUMS256.txt`, { signal: AbortSignal.timeout(30_000) })).text();
  } catch {
    fail(`拉取官方校验清单失败：${DIST_BASE}/SHASUMS256.txt`);
  }
  const line = sums.split("\n").find((l) => l.trimEnd().endsWith(`  ${sumKey}`));
  if (!line) fail(`官方校验清单中找不到 ${sumKey}`);
  const expected = line.trim().split(/\s+/)[0];
  if (actual !== expected) fail(`sha256 不符：期望 ${expected}，实际 ${actual}`);
  console.log(`[node-runtime:sync] sha256 校验通过（${sumKey}）`);
}

// Windows 用 PowerShell 解 zip（系统自带；PATH 里的 tar 可能是 GNU tar，不识别 zip 且会把
// 盘符冒号当远程主机）；Linux 用原生 tar 解 tar.gz。都在归档目录内以相对文件名调用。
function extract(archiveName, workDir) {
  const run =
    process.platform === "win32"
      ? spawnSync(
          "powershell",
          ["-NoProfile", "-Command", `Expand-Archive -LiteralPath '${archiveName}' -DestinationPath '.' -Force`],
          { cwd: workDir, stdio: ["ignore", "ignore", "pipe"] },
        )
      : spawnSync("tar", ["-xf", archiveName], { cwd: workDir, stdio: ["ignore", "ignore", "pipe"] });
  if (run.error) fail(`调用解包工具失败：${run.error.message}`);
  if (run.status !== 0) fail(`解包失败：${run.stderr?.toString().trim()}`);
}

try {
  fs.mkdirSync(destDir, { recursive: true });
  // 清掉崩溃残留的临时解包目录：bundle.resources 整目录打包，残留会原样进安装包。
  for (const name of fs.readdirSync(destDir)) {
    if (name.startsWith(".tmp-")) fs.rmSync(path.join(destDir, name), { recursive: true, force: true });
  }
  const workDir = fs.mkdtempSync(path.join(destDir, ".tmp-"));
  try {
    const archivePath = path.join(workDir, path.basename(target.archive));
    await downloadAndVerify(target.archive, target.sumKey, archivePath);
    extract(path.basename(archivePath), workDir);
    // 清掉另一平台留下的同名二进制：它会被 bundle.resources 整目录打包，白白多出一份。
    for (const name of target.staleBinaries) {
      fs.rmSync(path.join(destDir, name), { force: true });
    }
    fs.copyFileSync(path.join(workDir, target.members.binary.from), binaryDest);
    fs.copyFileSync(path.join(workDir, target.members.license.from), licenseDest);
    // Unix 下保持可执行位（Windows 无此概念）。
    if (process.platform !== "win32") fs.chmodSync(binaryDest, 0o755);
    fs.writeFileSync(
      versionJsonDest,
      JSON.stringify({ runtime: "node", version: NODE_VERSION }, null, 2) + "\n",
    );
    fs.writeFileSync(stampDest, stampText);
  } finally {
    fs.rmSync(workDir, { recursive: true, force: true });
  }
} catch (e) {
  const message = e instanceof Error ? e.message : String(e);
  console.error(message.startsWith("[node-runtime:sync] ") ? message : `[node-runtime:sync] ${message}`);
  process.exit(1);
}
console.log(
  `[node-runtime:sync] 已同步 Node ${NODE_VERSION} → ${path.relative(root, binaryDest)}`,
);
