/**
 * 把原生 esbuild 可执行文件同步到 `src-tauri/resources/esbuild/`。
 *
 * 为什么需要它：插件安装/更新时由宿主把声明的 npm 依赖打成自包含产物，打包器必须随应用分发
 * （用户机器不预装 Node）。二进制体积约 10MB，不入库（见 .gitignore），构建前从 esbuild 的
 * 平台包拷一份过去——因此构建机需要先 `pnpm install`。
 *
 * 目标目录始终存在（.gitkeep 入库）：`bundle.resources` 指向目录，缺少二进制时只让运行时报
 * 明确错误，而不是让 `cargo build` 直接失败。
 *
 * 入口：挂在 `pnpm run tauri:dev` / `tauri:build` 前置；直接用 `pnpm tauri build` 会绕过同步。
 */
import { createRequire } from "node:module";
import path from "node:path";
import fs from "node:fs";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const destDir = path.join(root, "src-tauri", "resources", "esbuild");
// 平台包内的可执行文件位置随平台不同：Windows 在包根（esbuild.exe），其余在 bin/ 下。
const candidates = ["bin/esbuild", "esbuild.exe", "esbuild"];
const destName = process.platform === "win32" ? "esbuild.exe" : "esbuild";
const staleNames = candidates.filter((name) => name !== destName);
const platformPkg = `@esbuild/${process.platform}-${process.arch}`;

function fail(message) {
  console.error(`[esbuild:sync] ${message}`);
  process.exit(1);
}

const require = createRequire(import.meta.url);

let esbuildPkgPath;
try {
  esbuildPkgPath = require.resolve("esbuild/package.json");
} catch {
  fail("未找到 esbuild 包，请先在项目根执行 pnpm install");
}

let platformPkgPath;
try {
  // 平台包是 esbuild 的可选依赖，从 esbuild 自身的 node_modules 解析（不依赖提升方式）。
  platformPkgPath = createRequire(esbuildPkgPath).resolve(`${platformPkg}/package.json`);
} catch {
  fail(`未找到 ${platformPkg}，请先执行 pnpm install（当前平台：${process.platform}-${process.arch}）`);
}

const pkgDir = path.dirname(platformPkgPath);
const source = candidates.map((name) => path.join(pkgDir, name)).find((p) => fs.existsSync(p));
if (!source) {
  fail(`平台包内未找到可执行文件，已查找：${candidates.map((n) => path.join(pkgDir, n)).join(" / ")}`);
}
// 平台包自身不带许可文件，从 esbuild 主包取一份随二进制一起分发（再分发必须带版权与许可声明）。
const license = path.join(path.dirname(esbuildPkgPath), "LICENSE.md");
if (!fs.existsSync(license)) {
  fail(`未找到 esbuild 许可文件：${license}（再分发需随包附带）`);
}

const dest = path.join(destDir, destName);
const licenseDest = path.join(destDir, "LICENSE.md");
const stamp = path.join(destDir, ".version");

let esbuildVersion;
let sourceSize;
try {
  esbuildVersion = JSON.parse(fs.readFileSync(esbuildPkgPath, "utf8")).version;
  sourceSize = fs.statSync(source).size;
} catch (e) {
  fail(`读取 esbuild 包信息失败：${e.message}`);
}
const stampText = `${esbuildVersion} ${sourceSize}`;

// 判「已是最新」要求三样都齐且二进制大小与源一致：只看版本戳会漏掉「许可没复制上来」与
// 「二进制被截断/替换」两种残缺——贴一份不完整的包比重新复制一次贵得多。
if (isUpToDate()) {
  console.log(`[esbuild:sync] 已是最新（${esbuildVersion}）`);
  process.exit(0);
}

/** 读侧任何异常都按「不是最新」处理，交给下面的复制路径兜底（不因此中断构建）。 */
function isUpToDate() {
  try {
    return (
      fs.statSync(dest).size === sourceSize &&
      fs.existsSync(licenseDest) &&
      fs.readFileSync(stamp, "utf8") === stampText
    );
  } catch {
    return false;
  }
}

try {
  fs.mkdirSync(destDir, { recursive: true });
  // 清掉另一平台留下的同名文件：它会被 bundle.resources 整目录打包，白白多出一份十几 MB。
  for (const name of staleNames) {
    fs.rmSync(path.join(destDir, name), { force: true });
  }
  fs.copyFileSync(source, dest);
  // Unix 下保持可执行位（Windows 无此概念）。
  if (process.platform !== "win32") fs.chmodSync(dest, 0o755);
  fs.copyFileSync(license, licenseDest);
  fs.writeFileSync(stamp, stampText);
} catch (e) {
  fail(`写入可执行文件失败：${e.message}`);
}
console.log(`[esbuild:sync] 已同步 esbuild ${esbuildVersion} → ${path.relative(root, dest)}`);
