# 当前验证记录

2026-10-17，Linux 构建环境：

- Rust 核心 36 项安全回归测试全部通过，包括“路由已写入后命令超时”和“回滚失败保留日志”的补充回归。
- Linux 桌面界面 `cargo check --locked` 通过。
- Windows x64 GNU 目标完整 GUI / 核心 `cargo check --locked --target x86_64-pc-windows-gnu` 通过。
- macOS ARM64 目标完整 GUI / 核心 `cargo check --locked --target aarch64-apple-darwin` 通过。
- Windows x64 GNU/LLVM Release 已在 Linux 使用 Rust 1.99.1 + Zig 0.17.0 成功交叉链接；禁用此交叉构建的 LTO。文件为 PE32+ x86-64，5.3 MiB；DLL 导入仅为 Windows 系统 / UCRT / OpenGL 组件，没有 MinGW 或 Rust 专用运行库 DLL。
- Windows EXE SHA-256：`100e88db3a23a0c5c2e381fd1c16b9c6e4a45f39b723737cb2a0a1686e2f6ba9`。
- 尚未完成 macOS 链接打包及 Windows/macOS 实机网络验收；交叉编译或链接通过不代表目标操作系统运行成功。

GitHub Actions 已触发，但 [运行 38993565849](https://github.com/aibalabala/TailscaleSplitGUI/actions/runs/38993565849) 的 core-tests 在执行任何 step 前失败，steps 为空，日志接口返回 BlobNotFound；desktop 被跳过。当前接口没有提供可确认的具体原因，不将其归因于源码、账单或权限中的任何一种。

当前可提供 **Windows 交叉编译 Alpha 包**，没有实机验收通过的稳定安装包；macOS Universal `.app` 尚未产出。双平台原生构建脚本和工作流已经提交，CI runner 问题排除后可重新运行；在两平台完成 ACCEPTANCE.md 前保持 Alpha / Draft。

交叉构建复现（Linux，预先安装 Zig 0.17.0）：

```bash
rustup target add x86_64-pc-windows-gnullvm
chmod +x cross-platform/scripts/zig-windows-linker.py
SPLITGUI_ZIG=/absolute/path/to/zig \
CARGO_PROFILE_RELEASE_LTO=false \
CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER="$PWD/cross-platform/scripts/zig-windows-linker.py" \
cargo build --release --locked --target x86_64-pc-windows-gnullvm --manifest-path cross-platform/Cargo.toml
```
