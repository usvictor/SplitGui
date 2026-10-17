use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub physical_interface: Option<String>,
    /// Each entry matches the name AND its descendants, on both operating systems.
    pub domestic_domains: Vec<String>,
    pub domestic_dns: Vec<Ipv4Addr>,
    /// Optional, explicitly imported China IPv4 CIDRs. No downloaded rules execute as code.
    pub china_routes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 2,
            physical_interface: None,
            domestic_domains: "126.com 163.com aliyun.cn aliyun.com apple.com.cn dola.com doubao.com ima.tencent.com mediatrack.cn baidu.com qq.com runninghub.cn workbuddy.cn www.geeknow.top taobao.com tmall.com jd.com bilibili.com".split_whitespace().map(str::to_owned).collect(),
            domestic_dns: vec![Ipv4Addr::new(223, 5, 5, 5), Ipv4Addr::new(119, 29, 29, 29)],
            china_routes: vec![],
        }
    }
}

pub fn normalize_domain(raw: &str) -> Result<String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    ensure!(
        d.len() <= 253 && d.contains('.') && d.parse::<Ipv4Addr>().is_err(),
        "请输入完整域名（不用 URL / 通配符），国际域名请使用 Punycode: {raw}"
    );
    for l in d.split('.') {
        ensure!(
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "无效域名: {raw}"
        );
    }
    Ok(d)
}

pub fn matches(name: &str, suffix: &str) -> bool {
    name == suffix || name.strip_suffix(suffix).is_some_and(|p| p.ends_with('.'))
}

pub fn protected_name(name: &str) -> bool {
    !name.contains('.')
        || [
            "local",
            "localhost",
            "lan",
            "home.arpa",
            "in-addr.arpa",
            "ip6.arpa",
            "ts.net",
            "internal",
            "test",
            "invalid",
            "example",
        ]
        .iter()
        .any(|d| matches(name, d))
}

impl Config {
    pub fn validate(&mut self) -> Result<()> {
        ensure!(self.version == 2, "不支持的配置版本");
        ensure!(
            !self.domestic_domains.is_empty() && self.domestic_domains.len() <= 2000,
            "国内域名数量应为 1–2000"
        );
        self.domestic_domains = self
            .domestic_domains
            .iter()
            .map(|d| normalize_domain(d))
            .collect::<Result<_>>()?;
        self.domestic_domains.sort();
        self.domestic_domains.dedup();
        for d in &self.domestic_domains {
            ensure!(
                !protected_name(d),
                "不能接管局域网 / Tailnet / 保留域名: {d}"
            );
        }
        ensure!(
            !self.domestic_dns.is_empty() && self.domestic_dns.len() <= 3,
            "需要 1–3 个国内 DNS IPv4"
        );
        for ip in &self.domestic_dns {
            ensure!(public_v4(*ip), "国内 DNS 必须是公网 IPv4: {ip}");
        }
        ensure!(self.china_routes.len() <= 20000, "CIDR 数量超过限制");
        for s in &self.china_routes {
            Cidr::parse_public(s)?;
        }
        self.china_routes.sort();
        self.china_routes.dedup();
        Ok(())
    }
    pub fn domestic(&self, name: &str) -> bool {
        let n = name.trim_end_matches('.').to_ascii_lowercase();
        !protected_name(&n) && self.domestic_domains.iter().any(|d| matches(&n, d))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cidr {
    pub base: u32,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Self> {
        let (ip, p) = s
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("CIDR 格式错误"))?;
        let ip: Ipv4Addr = ip.parse()?;
        let prefix: u8 = p.parse()?;
        ensure!(prefix <= 32, "CIDR 前缀错误");
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        ensure!(
            u32::from(ip) & mask == u32::from(ip),
            "CIDR 必须使用网络地址: {s}"
        );
        Ok(Self {
            base: u32::from(ip),
            prefix,
        })
    }
    pub fn parse_public(s: &str) -> Result<Self> {
        let c = Self::parse(s)?;
        for p in RESERVED {
            let p = Self::parse(p)?;
            let prefix = c.prefix.min(p.prefix);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            if c.base & mask == p.base & mask {
                bail!("禁止覆盖保护地址或默认路由: {s}");
            }
        }
        Ok(c)
    }
    pub fn contains(self, ip: Ipv4Addr) -> bool {
        let mask = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix)
        };
        u32::from(ip) & mask == self.base
    }
}

const RESERVED: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
];
pub fn public_v4(ip: Ipv4Addr) -> bool {
    !RESERVED
        .iter()
        .any(|s| Cidr::parse(s).unwrap().contains(ip))
}
