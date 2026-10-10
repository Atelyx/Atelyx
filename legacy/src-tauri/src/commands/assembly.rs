//! 应用装配版本：进程内单调计数器，组合装配输入的版本锚（两个 bump 源 = plugin 的
//! 插件行/组合用户层落盘事务与 global 的组合补丁命令；各窗口据此比对装配快照新旧）。
//! 应用重启后全部窗口重建重取基线，无需跨进程持久化。

/// 应用装配版本计数器。
static ASSEMBLY_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 自增并返回新版本号（写盘成功路径调用）。
pub fn bump_assembly_version() -> u64 {
    ASSEMBLY_VERSION.fetch_add(1, std::sync::atomic::Ordering::Release) + 1
}

/// 读当前版本号（广播与读取命令快照用）。
pub fn assembly_version() -> u64 {
    ASSEMBLY_VERSION.load(std::sync::atomic::Ordering::Acquire)
}

/// 测试专用闸锁：版本计数器是进程级全局静态，并行测试共享。凡断言版本或经实质变更前移
/// 版本的测试都先取它，把「读版本 → 触发变更 → 读版本」变成独占窗口——相等断言不被其他
/// 测试的并发 bump 打穿（bump 源：plugin 的状态事务回归、global 的补丁测试）。
#[cfg(test)]
pub(crate) static ASSEMBLY_TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
