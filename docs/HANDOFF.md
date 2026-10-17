# Rust 迁移交付

项目现为 Windows 原生 Rust/egui 应用。Luna 完成实现和环境配置，主代理负责原代码审计、实施指导、独立验收和最终提交。

## 启动

运行仓库下 `dist/AnimRef/AnimRef.exe`。完整分发包为 `dist/AnimRef-Windows.zip`，解压后保持 `tools` 和 `docs` 目录与 exe 相邻。Windows 10/11 x64 需要 Microsoft Visual C++ x64 运行库；当前机器已具备。

## 功能与数据

- 支持图片、GIF、文字、本地视频、远程图片、YouTube 片段，以及公开 Pinterest Pin 的图片和视频导入；视频保持原版静音参考播放方式。
- 支持画布缩放、平移、选择、层级、拖动、等比调整尺寸、删除和区间循环。新版取消独立的视频编辑模式，选中动态项目后直接使用简洁播放条。
- 画布不保留顶部菜单、属性栏和状态栏；窗口标题及控制按钮在顶部悬停时显示，主要动作通过右键菜单和快捷键完成。
- 复制仅写入系统剪贴板；导出保留图片、GIF 或视频的实际媒体格式。暂停状态、当前播放位置、原始来源链接及点阵网格吸附设置随场景保存。媒体右键菜单可用系统默认浏览器打开原始来源。
- 保留无边框置顶窗口及常用快捷键；快捷键详见根目录 Readme。
- 可读取旧 `.purgif`，保留画布变换、元素顺序、中文/多行文字和循环区间。旧版编辑视频状态恢复为正常画布视图。
- YouTube 先读取元数据，填写原视频起止时间，点击下载后请求该区间；原链接、原时长、区间和缓存路径写入场景。网络分片/关键帧可能带来少量相邻数据读取。
- `.purgif` 不打包外部媒体。移动场景时需保留对应文件；换机器或清空缓存后，YouTube 片段可重新选择下载。

旧 Electron/Node 运行文件、npm 清单/锁文件、旧展示素材和对应忽略规则已清理。`LICENSE` 和通用 Git 属性保留；媒体侧程序许可证在 `tools/licenses`。

## 开发与验收

```powershell
./scripts/setup-windows.ps1
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
./scripts/package-windows.ps1
```

准确版本见 [环境记录](ENVIRONMENT.md)。原迁移的测试结果见 [迁移验收记录](ACCEPTANCE.md)，界面及播放系统调整见 [修订验收记录](REVISION_ACCEPTANCE.md)，最新 2.1.1 结果见 [UI 与 Pinterest 验收](UI_POLISH_ACCEPTANCE.md)。`target/acceptance` 中的合成场景、运行报告与诊断图片用于本机复验；生成物和 `dist` 均不纳入 Git。

最终提交可通过 `git log -1 --oneline` 查看。本次只提交本地仓库，未推送远程。
