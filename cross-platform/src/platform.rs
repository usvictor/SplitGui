use crate::{
    config::Config,
    routes::{Network, Route, RouteBackend},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{
    io::Read,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const OWNER: &str = "TailscaleSplitGUI-v2";

pub fn run(exe: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; never invoke a shell for arguments
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("启动失败: {}", exe.display()))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let err = thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("命令超时（未确认成功）: {}", exe.display());
        }
        thread::sleep(Duration::from_millis(30));
    };
    let output = String::from_utf8_lossy(
        &out.join()
            .map_err(|_| anyhow::anyhow!("stdout 线程失败"))??,
    )
    .into_owned();
    let errors = String::from_utf8_lossy(
        &err.join()
            .map_err(|_| anyhow::anyhow!("stderr 线程失败"))??,
    )
    .into_owned();
    ensure!(
        status.success(),
        "{}: {} {}",
        exe.display(),
        errors.trim(),
        output.trim()
    );
    Ok(output.trim().trim_start_matches('\u{feff}').into())
}

pub fn powershell(script: &str) -> Result<String> {
    #[cfg(windows)]
    let exe = crate::win_paths::powershell()?;
    #[cfg(not(windows))]
    let exe = PathBuf::from("/nonexistent/windows-powershell");
    // All dynamic data is validated or single-quoted with apostrophes escaped.
    run(
        &exe,
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &powershell_script(script),
        ],
    )
}
/// Both normal output and exception messages must use UTF-8 when captured by Rust.
pub fn powershell_script(script: &str) -> String {
    format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $OutputEncoding=[Console]::OutputEncoding; $env:PSModulePath=[IO.Path]::Combine($PSHOME,'Modules'); try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}")
}
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        return crate::win_paths::known_folder(
            &windows_sys::Win32::UI::Shell::FOLDERID_ProgramData,
        )
        .expect("Windows ProgramData unavailable; refusing to guess a privileged path")
        .join("TailscaleSplitGUI-v2");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/Library/Application Support/TailscaleSplitGUI-v2");
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        PathBuf::from("/var/lib/tailscale-splitgui-v2")
    }
}

pub fn tailscale_binary() -> Result<PathBuf> {
    #[cfg(windows)]
    let paths =
        [
            crate::win_paths::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_ProgramFiles)?
                .join("Tailscale")
                .join("tailscale.exe"),
        ];
    #[cfg(not(windows))]
    let paths = [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ];
    paths
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow::anyhow!("找不到 Tailscale，请先安装官方客户端"))
}

pub fn exit_node() -> Result<String> {
    let bin = tailscale_binary()?;
    let v: Value = serde_json::from_str(&run(&bin, &["status", "--json"])?)?;
    ensure!(v["BackendState"] == "Running", "Tailscale 未连接");
    let exit = &v["ExitNodeStatus"];
    ensure!(
        exit["Online"].as_bool() == Some(true),
        "当前出口节点离线 / 无法确认在线"
    );
    let id = exit["ID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("没有已选中的出口节点")?;
    let prefs: Value = serde_json::from_str(&run(&bin, &["debug", "prefs"])?)?;
    ensure!(
        prefs["ExitNodeAllowLANAccess"].as_bool() == Some(true),
        "请先在 Tailscale 开启 Allow Local Network Access（允许局域网访问），当前设置未被修改"
    );
    Ok(id.into())
}

pub fn networks() -> Result<Vec<Network>> {
    #[cfg(windows)]
    {
        let s = powershell("$items=@(Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | ForEach-Object { $c=Get-NetIPConfiguration -InterfaceIndex $_.ifIndex; if($c.IPv4DefaultGateway -and $c.IPv4Address){ [pscustomobject]@{interface=[string]$_.ifIndex;gateway=[string]$c.IPv4DefaultGateway.NextHop;address=[string]$c.IPv4Address[0].IPAddress} } }); ConvertTo-Json -InputObject $items -Compress")?;
        return Ok(serde_json::from_str(&s)?);
    }
    #[cfg(target_os = "macos")]
    {
        let ports = run(
            Path::new("/usr/sbin/networksetup"),
            &["-listallhardwareports"],
        )?;
        let mut out = vec![];
        for line in ports.lines() {
            if let Some(dev) = line.strip_prefix("Device: ") {
                if !dev.starts_with("en") || !dev[2..].bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                let address = run(Path::new("/usr/sbin/ipconfig"), &["getifaddr", dev]);
                let gateway = run(
                    Path::new("/usr/sbin/ipconfig"),
                    &["getoption", dev, "router"],
                );
                if let (Ok(a), Ok(g)) = (address, gateway) {
                    if let (Ok(address), Ok(gateway)) = (a.parse(), g.parse()) {
                        out.push(Network {
                            interface: dev.into(),
                            gateway,
                            address,
                        });
                    }
                }
            }
        }
        return Ok(out);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    bail!("实机网络操作仅支持 Windows / macOS");
}

pub fn select_network(config: &Config) -> Result<Network> {
    let mut n = networks()?;
    if let Some(i) = &config.physical_interface {
        n.retain(|n| &n.interface == i);
    }
    ensure!(
        n.len() == 1,
        "无法唯一确定物理网络（{} 个候选）；请在设置中选择网卡",
        n.len()
    );
    let n = n.remove(0);
    ensure!(
        !n.gateway.is_unspecified() && !n.address.is_unspecified(),
        "无效物理网络"
    );
    Ok(n)
}

pub struct NativeRoutes;

impl RouteBackend for NativeRoutes {
    fn is_local(&mut self, ip: Ipv4Addr, network: &Network) -> Result<bool> {
        Ok(route_interface(ip)? == network.interface)
    }
    fn exact_exists(&mut self, route: &Route) -> Result<bool> {
        #[cfg(windows)]
        {
            return crate::win_routes::exists(route);
        }
        #[cfg(target_os = "macos")]
        {
            let table = run(Path::new("/usr/sbin/netstat"), &["-rn", "-f", "inet"])?;
            return Ok(table.lines().any(|l| {
                l.split_whitespace()
                    .next()
                    .is_some_and(|s| mac_destination_matches(s, &route.destination))
            }));
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = route;
            bail!("不支持的操作系统");
        }
    }
    fn add(&mut self, route: &Route) -> Result<()> {
        crate::config::Cidr::parse_public(&route.destination)?;
        #[cfg(windows)]
        {
            crate::win_routes::add(route)?;
        }
        #[cfg(target_os = "macos")]
        {
            run(
                Path::new("/sbin/route"),
                &[
                    "-n",
                    "add",
                    "-net",
                    &route.destination,
                    &route.network.gateway.to_string(),
                ],
            )?;
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        bail!("不支持的操作系统");
        #[allow(unreachable_code)]
        Ok(())
    }
    fn remove_owned(&mut self, route: &Route) -> Result<()> {
        crate::config::Cidr::parse_public(&route.destination)?;
        #[cfg(windows)]
        {
            crate::win_routes::remove(route)?;
        }
        #[cfg(target_os = "macos")]
        {
            let table = run(Path::new("/usr/sbin/netstat"), &["-rn", "-f", "inet"])?;
            let owned = table.lines().any(|l| {
                let p: Vec<_> = l.split_whitespace().collect();
                p.len() >= 4
                    && mac_destination_matches(p[0], &route.destination)
                    && p[1] == route.network.gateway.to_string()
                    && p.iter().any(|s| *s == route.network.interface)
            });
            if owned {
                run(
                    Path::new("/sbin/route"),
                    &[
                        "-n",
                        "delete",
                        "-net",
                        &route.destination,
                        &route.network.gateway.to_string(),
                    ],
                )?;
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        bail!("不支持的操作系统");
        #[allow(unreachable_code)]
        Ok(())
    }
}

pub fn mac_destination_matches(text: &str, prefix: &str) -> bool {
    let (addr, explicit) = text
        .split_once('/')
        .map(|(a, p)| (a, Some(p)))
        .unwrap_or((text, None));
    let mut parts: Vec<_> = addr.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return false;
    }
    let inferred = (parts.len() * 8).to_string();
    while parts.len() < 4 {
        parts.push("0");
    }
    let canonical = format!("{}/{}", parts.join("."), explicit.unwrap_or(&inferred));
    match (
        crate::config::Cidr::parse(&canonical),
        crate::config::Cidr::parse(prefix),
    ) {
        (Ok(a), Ok(b)) => a.base == b.base && a.prefix == b.prefix,
        _ => false,
    }
}

pub fn route_interface(ip: Ipv4Addr) -> Result<String> {
    #[cfg(windows)]
    {
        return crate::win_routes::interface(ip);
    }
    #[cfg(target_os = "macos")]
    {
        let s = run(Path::new("/sbin/route"), &["-n", "get", &ip.to_string()])?;
        return s
            .lines()
            .find_map(|l| l.trim().strip_prefix("interface: ").map(str::to_owned))
            .context("无法读取实际路由");
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = ip;
        bail!("不支持的操作系统");
    }
}

pub fn overseas_route() -> Result<String> {
    let interface = route_interface(Ipv4Addr::new(1, 1, 1, 1))?;
    #[cfg(target_os = "macos")]
    ensure!(
        interface.starts_with("utun"),
        "海外探针未走隧道，拒绝宣称分流成功"
    );
    #[cfg(windows)]
    {
        let index: u32 = interface.parse()?;
        let description = powershell(&format!(
            "(Get-NetAdapter -InterfaceIndex {index} -IncludeHidden).InterfaceDescription"
        ))?;
        ensure!(
            description.to_ascii_lowercase().contains("tailscale"),
            "海外探针没有走 Tailscale"
        );
    }
    Ok(interface)
}

pub fn dns_rule_text(domain: &str) -> String {
    format!(
        "# {OWNER}\ndomain {domain}\nnameserver 127.0.0.1\nport 53\nsearch_order 1\ntimeout 5\n"
    )
}

/// Detect conflicts before any scoped DNS mutation; never overwrite another owner.
pub fn preflight_dns(domains: &[String]) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        for d in domains {
            ensure!(
                !Path::new("/etc/resolver").join(d).exists(),
                "已有 resolver 文件，拒绝覆盖: {d}"
            );
        }
    }
    #[cfg(windows)]
    {
        let s = powershell("ConvertTo-Json -InputObject @(Get-DnsClientNrptPolicy -Effective | ForEach-Object {$_.Namespace}) -Compress")?;
        let names: Vec<String> = serde_json::from_str(&s)?;
        for d in domains {
            for existing in &names {
                let e = existing.trim_start_matches('.').to_ascii_lowercase();
                // Tailscale's root policy is expected; more-specific rules conflict.
                if !e.is_empty() {
                    ensure!(
                        !crate::config::matches(d, &e) && !crate::config::matches(&e, d),
                        "已有 DNS 策略冲突: {existing}"
                    );
                }
            }
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = domains;
        bail!("不支持的操作系统");
    }
    #[allow(unreachable_code)]
    Ok(())
}

pub fn install_dns(domain: &str) -> Result<String> {
    ensure!(
        crate::config::normalize_domain(domain)? == domain,
        "域名不是规范格式"
    );
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        std::fs::create_dir_all("/etc/resolver")?;
        let content = dns_rule_text(domain);
        let path = Path::new("/etc/resolver").join(domain);
        let staging =
            Path::new("/etc/resolver").join(format!(".splitgui-{:016x}", rand::random::<u64>()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        let result = (|| -> Result<()> {
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            // Atomic exclusive publication; neither a partial resolver file nor
            // an overwrite of a concurrently created rule is possible.
            std::fs::hard_link(&staging, &path)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(staging);
        result?;
        ensure!(
            std::fs::read_to_string(path)? == content,
            "DNS 规则写入后不匹配"
        );
        return Ok(content);
    }
    #[cfg(windows)]
    {
        let id = powershell(&format!("(Add-DnsClientNrptRule -Namespace {},{} -NameServers '127.0.0.1' -Comment '{OWNER}' -DisplayName {} -PassThru).Name", ps_quote(domain), ps_quote(&format!(".{domain}")), ps_quote(&format!("{OWNER}:{domain}"))))?;
        ensure!(!id.is_empty(), "NRPT 未返回规则 ID");
        return Ok(id);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    bail!("不支持的操作系统");
}

pub fn remove_dns(domain: &str, identity: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let _ = identity;
    ensure!(
        crate::config::normalize_domain(domain)? == domain,
        "无效域名"
    );
    #[cfg(target_os = "macos")]
    {
        let path = Path::new("/etc/resolver").join(domain);
        if path.exists() {
            let current = std::fs::read_to_string(&path)?;
            ensure!(
                current == dns_rule_text(domain),
                "DNS 文件已被其他程序修改，未删除: {domain}"
            );
            std::fs::remove_file(path)?;
        }
    }
    #[cfg(windows)]
    {
        // Match exact owner, domain, namespace and server even for write-ahead recovery.
        powershell(&format!("Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{OWNER}' -and $_.DisplayName -eq {} -and ($_.NameServers -join ',') -eq '127.0.0.1' -and ($_.Namespace -contains {}) -and ($_.Namespace -contains {}) -and $_.Namespace.Count -eq 2 {} }} | Remove-DnsClientNrptRule -Force", ps_quote(&format!("{OWNER}:{domain}")), ps_quote(domain), ps_quote(&format!(".{domain}")), if identity == "pending" { String::new() } else { format!("-and $_.Name -eq {}", ps_quote(identity)) }))?;
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = identity;
        bail!("不支持的操作系统");
    }
    #[allow(unreachable_code)]
    Ok(())
}

pub fn flush_dns() -> Result<()> {
    #[cfg(windows)]
    {
        powershell("Clear-DnsClientCache")?;
    }
    #[cfg(target_os = "macos")]
    {
        run(Path::new("/usr/bin/dscacheutil"), &["-flushcache"])?;
        run(Path::new("/usr/bin/killall"), &["-HUP", "mDNSResponder"])?;
    }
    Ok(())
}
