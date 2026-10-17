# 当前验证记录

2026-10-17，GitHub Actions 原生构建已通过：
[运行 39029040119](https://github.com/usvictor/SplitGui/actions/runs/39029040119)。

源提交：`2eeb0ca91610f966ed7ede50b52dd5eeda75bfc9`，Rust 1.99.1。

- Ubuntu：格式检查通过，36 项安全回归测试通过。
- Windows x64 MSVC：36 项测试通过，Release EXE 原生编译和 ZIP 打包成功。
- macOS Apple Silicon 运行器：36 项测试通过，ARM64 和 Intel x64 Release 编译成功，lipo 合并 Universal app 成功；ad-hoc 签名及 codesign --verify --deep --strict 校验通过。
- 两个平台的 artifact 均已成功上传。未完成真实设备 GUI、DNS、Tailscale 出口及 LAN 联合验收；仍为 Alpha。测试使用假 DNS 上游和假路由后端，不证明真实分流已通过。
- Windows 包未做发布者代码签名；macOS 包未做 Apple 公证。

## 下载

下载 Artifact 后解压，再解压其中的平台 ZIP。

| 平台 | Artifact | 外层 Artifact ZIP SHA-256 |
|---|---|---|
| Windows x64 | [SplitGUI-v2-Windows](https://github.com/usvictor/SplitGui/actions/runs/39029040119/artifacts/12100110294) | `0406dda5549bcb4b2fff5121e2d56bebaa3aa21ce48a77b770334710d7c36493` |
| macOS Intel / Apple Silicon | [SplitGUI-v2-macOS](https://github.com/usvictor/SplitGui/actions/runs/39029040119/artifacts/12099632930) | `2fa1146d91581f6ebf258c37612a5524932df966c884d5053067b521b2c1267a` |

Actions 当前保留期为 30 天，以上产物预计于 2026-11-16 到期，之后可重新构建。

## 历史说明

原仓库 aibalabala/TailscaleSplitGUI 的早期 Actions 在执行步骤前失败，原因未确认；不可据此认定私有仓库不能编译。本仓库首次原生构建已成功，替代此前仅有 Linux 交叉编译 Windows 包的交付状态。

真实网络验收按 [ACCEPTANCE.md](ACCEPTANCE.md) 执行。
