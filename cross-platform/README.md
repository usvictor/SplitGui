# SplitGUI v2 — Windows / macOS Rust 重构

当前版本是 **2.0.0 Alpha**，不是已经完成实机网络验收的稳定版。
本仓库迁入 Rust v2，使用同一份 Rust 核心和 egui/eframe 桌面界面。旧 Swift 版仍位于原 TailscaleSplitGUI 仓库。

## 使用

1. 从本分支 GitHub Actions 的 `Cross-platform Rust build` 成功运行中下载对应 artifact。Windows 为 x64 EXE；macOS 为 Intel / Apple Silicon Universal `.app`。
2. 先停止旧版 SplitGUI；Tailscale 选择原有 Exit Node，并开启 Allow Local Network Access。v2 不替换或清空出口节点。
3. 打开 v2，检查物理网卡、国内域名及国内 DNS。多网卡时必须明确选择。macOS 自动发现目前要求网卡能通过 DHCP 提供网关；静态地址网络无法识别时会拒绝启用。
4. 点击“启用 / 应用设置”，完成 UAC / macOS 管理员授权。未通过全部预检前不显示 active。
5. 等待系统解析命中本地 DNS、返回 IPv4 的本地路由和海外探针路由通过验证。`degraded / error / unknown` 不是成功。
6. 关闭窗口保留后台运行；“停止 / 恢复原网络”撤销本程序规则，并取消开机启动。

自用包未做 Windows 代码签名 / Apple 公证；macOS 包只作 ad-hoc 签名。请从自己的仓库构建记录下载并核对来源。

## DNS 与路由策略

| 请求 | 解析路径 | 路由行为 |
|---|---|---|
| 配置的国内域名及其子域 | Windows NRPT / macOS `/etc/resolver` → `127.0.0.1:53` → 国内 DNS | 国内 DNS 服务器和每个有效 A 地址先验证本地路由，再回答客户端 |
| 其他公网域名 | 保留原系统 / Tailscale DNS | 保留原海外出口；不新增海外 DNS 转发 |
| `.local`、单标签名、`home.arpa`、Tailnet 名、反向查询 | 不创建国内规则 | 不覆盖 LAN、CGNAT、Tailnet 和默认路由 |
| 国内 DNS 超时、空响应、异常地址或路由失败 | 仅重试配置中的国内 DNS；最终 SERVFAIL | 不回退到海外 DNS，不修改出口节点，不把失败标为成功 |

**域名与 IP 两层规则**：默认预置常用国内域名，支持导入旧版列表。原有中国 IPv4 路由保持不变；新装 Windows 如需完整的 IP 级中国分流，需在“China IPv4”区域导入核对过的 CIDR 列表。未导入时，v2 提供配置域名的动态直连，不能声称覆盖所有国内网站。CIDR 会拒绝覆盖默认、私网、CGNAT、保留网段。

域名规则使用 DNS 标签边界，例如 `qq.com` 包含 `a.qq.com`，不包含 `evilqq.com`。旧版精确域名迁移后扩展为该名称及子域，UI 显式说明；导入需要点击，默认不会覆盖用户文件。不要把企业内部公有后缀加入国内列表。

## 缓存与动态地址

- 上游 UDP 响应检查来源、事务 ID、问题、类和操作码；TC 截断后使用 TCP 重试。
- CNAME 必须在同一响应中提供完整链，拒绝循环、冲突和私网目标；只采纳链上有效 A 记录，不采纳额外记录。
- 缓存 TTL 取 CNAME/A 最小 TTL，最长 60 秒，绝不延长；TTL=0 不缓存；缓存最多 2048 个域名。
- 每次缓存命中仍核验真实路由。新地址先添加和验证后才返回；旧地址按最后一次回答的 TTL 加 30 秒缓冲到期清理，共享 IP 采用最长租约。
- 失败不立即删除已有正常路由。已有租约正常到期后清理，下一次有效查询会重新建立。
- 网络 / 出口变化会终止当前会话并恢复本程序改动，用户可重新启用；不会自动更换出口节点。
- 整体健康检查在后台线程执行，不阻塞 DNS 监听。Windows 查询和写路由使用原生 IP Helper API，避免逐个 DNS 请求启动 PowerShell。

## 范围与真实限制

- 当前国内域名采用 **IPv4 优先策略**：AAAA / HTTPS / SVCB 返回无数据，避免 IPv6 与 address hint 绕过验证；A 之外其他类型返回 REFUSED。不是全类型通用 DNS 代理，不适合将邮件、服务发现或 DNSSEC 必须验证的域交给本规则。
- 不设置 AD 位，不声称做了 DNSSEC 验证。国内上游为 UDP/TCP 明文 DNS，不声称加密 DNS或零泄漏。
- 浏览器自带 DoH、应用代理、独立 DNS 和未过期的应用缓存可绕过系统策略；需关闭独立 DNS、清理应用缓存后测试。系统检查不能替代每个应用的端到端验证。
- 路由按 IP 生效，共享 CDN IP 的其他域名可能同时直连。这是 IP 路由分流的固有限制；若要求同 IP 按域名严格隔离，需要应用代理架构。
- macOS 的海外探针验证隧道接口及 Tailscale 在线状态；多 VPN 并存时仍需实机核对抓包，不能将任意 utun 等同为证明所有应用通过 Tailscale。
- 大规模 China CIDR 首次写入会较慢，采用逐条写前日志与验证；不会为了速度跳过回滚日志。

## 权限、崩溃与恢复

GUI 普通权限运行，只允许固定 `install` / `disable` 命令通过操作系统提权，无管理员密码存储，无任意命令 RPC。后台文件保存到受保护目录：

- Windows：系统 ProgramData 下的 `TailscaleSplitGUI-v2`（通常为 `C:\ProgramData\TailscaleSplitGUI-v2`）；SYSTEM 计划任务。系统目录、Program Files 和用户配置目录通过 Windows API 读取，不假设系统盘为 C 盘。
- macOS：`/Library/Application Support/TailscaleSplitGUI-v2`；root LaunchDaemon。

两平台均先绑定回环 UDP 和 TCP 53 端口，端口占用则不修改系统 DNS。按域名的 DNS 规则不会改变全局网卡 DNS、`accept-dns`、系统代理或 Tailnet 全局配置。已有同域规则冲突时拒绝覆盖。

独立 supervisor 监视 worker，worker 异常退出后按日志先删除本程序 DNS 规则、刷新系统缓存，再清理本程序路由。只删除仍匹配所有权信息的规则；其他程序修改过的配置不强行删除，会报告恢复未完成。崩溃重启时从 write-ahead journal 恢复；操作系统任务恢复 supervisor 非正常退出。停止动作不会强杀卡住的恢复流程。

不要在分流启用时手工删除后台文件。GUI 无法启动时，以管理员运行原包中的：

```text
TailscaleSplitGUI.exe disable
```

macOS：

```bash
sudo /Applications/TailscaleSplitGUI.app/Contents/MacOS/TailscaleSplitGUI disable
```

## 构建与验证

需要 Rust stable。依赖版本由 Cargo.lock 固定。

```bash
cargo test --locked --no-default-features --manifest-path cross-platform/Cargo.toml
cargo run --locked --manifest-path cross-platform/Cargo.toml
```

Windows 执行 `cross-platform/scripts/build-windows.ps1`；macOS 执行 `bash cross-platform/scripts/build-macos.sh`。

当前还提供 Linux 交叉构建的 Windows x64 Alpha ZIP。该包使用 Rust 1.99.1 / Zig 0.17.0 / UCRT，已成功链接并核对 PE64 格式与系统 DLL 依赖，尚未进行 Windows 实机运行验收。构建记录见 `VALIDATION.md`。

测试覆盖空响应、超时、异常地址、CNAME、过期 / 零 TTL、缓存路由复验、跨策略泄漏、IPv6 hint、UDP 截断、受保护网络、路由所有权和失败回滚。测试使用假上游和假路由后端，不修改测试机网络。CI 编译 / 单元测试通过不等于实机分流已通过。

实机验收清单见 `ACCEPTANCE.md`。

实现参考：[Microsoft NRPT](https://learn.microsoft.com/powershell/module/dnsclient/add-dnsclientnrptrule)、[IP Helper GetBestRoute2](https://learn.microsoft.com/windows/win32/api/netioapi/nf-netioapi-getbestroute2)、[Tailscale Exit Nodes](https://tailscale.com/docs/features/exit-nodes)、macOS 本机 `man 5 resolver`。
