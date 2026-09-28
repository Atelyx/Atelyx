/**
 * 校验安卓 native 库的 16KB 内存页对齐。
 *
 * 为什么需要它：Android 15+ 设备的系统内存页可能为 16KB，未按 16KB 对齐的 `.so` 会直接加载失败；
 * 而对齐由链接器参数决定（NDK r28+ 对 64 位 ABI 默认对齐，32 位 ABI 不会），
 * 环境变量 RUSTFLAGS 会整体覆盖 `.cargo/config.toml` 的那份配置，因此由 `src-tauri/build.rs`
 * 改用 `cargo:rustc-link-arg` 追加 `max-page-size`，最终以本脚本对构建产物的校验为准。
 *
 * 判定依据（每个 PT_LOAD 段同时满足）：
 * 1. `p_align >= 16384`：链接器按 16KB 粒度对齐；
 * 2. `(p_vaddr - p_offset) % 16384 == 0`：加载地址与文件偏移同余——加载器按页映射需要这个关系。
 *    注意**不是**要求 `p_vaddr` 本身是 16KB 的整数倍（段可落在页内任意同余位置）。
 *
 * 默认校验 `src-tauri/gen/android/app/src/main/jniLibs/<abi>/*.so`（构建产物）。
 *
 * 用法：node scripts/check-android-16kb.mjs [目标目录]
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const PAGE_SIZE = 16384;
const PT_LOAD = 1;
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const targetDir = process.argv[2]
  ? path.resolve(process.argv[2])
  : path.join(root, "src-tauri", "gen", "android", "app", "src", "main", "jniLibs");

function fail(message) {
  console.error(`[android:16kb] ${message}`);
  process.exit(1);
}

/** 解析 ELF 的程序头，返回每个 PT_LOAD 段的 { vaddr, offset, align }；非 ELF / 截断则抛错。 */
function loadSegments(file) {
  const buf = fs.readFileSync(file);
  if (buf.length < 0x40) throw new Error("文件过短，不是有效 ELF");
  if (buf[0] !== 0x7f || buf[1] !== 0x45 || buf[2] !== 0x4c || buf[3] !== 0x46) {
    throw new Error("魔数不是 ELF");
  }
  const is64 = buf[4] === 2;
  if (buf[5] !== 1) throw new Error("仅支持小端 ELF（安卓全部 ABI 均为小端）");

  const phoff = is64 ? Number(buf.readBigUInt64LE(0x20)) : buf.readUInt32LE(0x1c);
  const phentsize = is64 ? buf.readUInt16LE(0x36) : buf.readUInt16LE(0x2a);
  const phnum = is64 ? buf.readUInt16LE(0x38) : buf.readUInt16LE(0x2c);

  const segments = [];
  for (let i = 0; i < phnum; i++) {
    const off = phoff + i * phentsize;
    if (off + phentsize > buf.length) throw new Error("程序头越界");
    const type = buf.readUInt32LE(off);
    if (type !== PT_LOAD) continue;
    segments.push(
      is64
        ? {
            offset: Number(buf.readBigUInt64LE(off + 8)),
            vaddr: Number(buf.readBigUInt64LE(off + 16)),
            align: Number(buf.readBigUInt64LE(off + 48)),
          }
        : {
            offset: buf.readUInt32LE(off + 4),
            vaddr: buf.readUInt32LE(off + 8),
            align: buf.readUInt32LE(off + 28),
          },
    );
  }
  return segments;
}

/** 单个 LOAD 段是否满足 16KB 页对齐（见文件头注释的两条判据）。 */
function segmentAligned({ vaddr, offset, align }) {
  if (align < PAGE_SIZE) return false;
  return (((vaddr - offset) % PAGE_SIZE) + PAGE_SIZE) % PAGE_SIZE === 0;
}

if (!fs.existsSync(targetDir)) {
  fail(`目录不存在：${targetDir}（先构建安卓产物：pnpm tauri android build）`);
}
if (!fs.statSync(targetDir).isDirectory()) {
  fail(`目标不是目录：${targetDir}（传入目录即可；单个库请传其所在目录）`);
}

/** 某目录下的 .so 文件名（不存在目录视为空）。 */
function soFiles(dir) {
  return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith(".so")).sort() : [];
}

// 分组 = 目标目录自身（直接含 .so 时）+ 各子目录（jniLibs 的 <abi>/ 布局）。
// 混合目录（如 target/<triple>/debug 同时含根级 .so 与 deps/）两边都查，不静默跳过根级库。
const subDirs = fs
  .readdirSync(targetDir, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .sort();
const groups = [
  ...(soFiles(targetDir).length > 0 ? [{ abi: path.basename(targetDir), dir: targetDir }] : []),
  ...subDirs.map((abi) => ({ abi, dir: path.join(targetDir, abi) })),
];

const checked = [];
let failures = 0;
for (const { abi, dir } of groups) {
  for (const name of soFiles(dir)) {
    const file = path.join(dir, name);
    let segments;
    try {
      segments = loadSegments(file);
    } catch (e) {
      console.error(`  失败 ${abi}/${name}：${e.message}`);
      failures++;
      continue;
    }
    if (segments.length === 0) {
      console.error(`  失败 ${abi}/${name}：没有可加载段`);
      failures++;
      continue;
    }
    const bad = segments.filter((segment) => !segmentAligned(segment));
    if (bad.length > 0) {
      console.error(`  失败 ${abi}/${name}：${bad.length} 个 LOAD 段未按 16KB 对齐`);
      failures++;
    } else {
      checked.push(`${abi}/${name}`);
    }
  }
}

if (checked.length === 0 && failures === 0) fail(`没有找到任何 .so：${targetDir}`);
if (failures > 0) fail(`${failures} 个库未通过 16KB 对齐校验`);
console.log(`[android:16kb] 通过：${checked.length} 个库（${checked.join("、")}）`);
