# SplitGui

Windows / macOS 的 Tailscale 域名 DNS 与路由分流工具，Rust + egui。

当前为 **2.0.0 Alpha**，尚未完成真实设备网络验收。

- [使用方法、策略与限制](cross-platform/README.md)
- [实机验收清单](cross-platform/ACCEPTANCE.md)
- [GitHub Actions 构建](https://github.com/usvictor/SplitGui/actions/workflows/cross-platform.yml)

成功构建的 Artifacts 包含 Windows x64 EXE 与 macOS Intel / Apple Silicon Universal app。下载 Artifact 后解压，再解压其中的平台 ZIP。

代码迁自 aibalabala/TailscaleSplitGUI 的 Rust 重构分支，源提交 `85ed941d2010aade8888a7dff54c2ae043fc1e28`。
