# 集成测试说明

- 本目录的测试是独立 crate：链接 `atelyx` 的普通（非 test）构建产物，
  因此 `app_data_dir()` 里的 `#[cfg(test)]` 隔离分支在这里**不生效**，
  这类测试一旦真正读取应用数据目录就会碰到用户真实的 `com.atelyx.desktop`。
- 需要隔离目录的用例请自行用 `std::env::temp_dir()` 下的临时路径，不要依赖库内的单测分支。
