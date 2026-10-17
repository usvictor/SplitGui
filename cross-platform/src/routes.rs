use crate::config::{public_v4, Cidr};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    pub interface: String,
    pub gateway: Ipv4Addr,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub destination: String,
    pub network: Network,
}

pub trait RouteBackend {
    fn is_local(&mut self, ip: Ipv4Addr, network: &Network) -> Result<bool>;
    fn exact_exists(&mut self, route: &Route) -> Result<bool>;
    fn add(&mut self, route: &Route) -> Result<()>;
    /// Remove only if destination, gateway AND interface still match.
    fn remove_owned(&mut self, route: &Route) -> Result<()>;
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub routes: Vec<Route>,
    pub dns: BTreeMap<String, String>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temp = path.with_extension("new");
    {
        let mut f = std::fs::File::create(&temp)?;
        use std::io::Write;
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
    }
    // Windows rename cannot replace an existing file. Keep a recovery copy until replacement completes.
    let backup = path.with_extension("bak");
    if path.exists() {
        std::fs::copy(path, &backup)?;
        #[cfg(windows)]
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temp, path)?;
    if backup.exists() {
        std::fs::remove_file(backup)?;
    }
    Ok(())
}

impl Journal {
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            return Ok(serde_json::from_slice(&std::fs::read(path)?)?);
        }
        let backup = path.with_extension("bak");
        if backup.exists() {
            return Ok(serde_json::from_slice(&std::fs::read(backup)?)?);
        }
        Ok(Self::default())
    }
}

pub struct Routes<B: RouteBackend> {
    pub backend: B,
    pub network: Network,
    pub journal: Journal,
    pub path: PathBuf,
    leases: BTreeMap<String, u64>,
}

impl<B: RouteBackend> Routes<B> {
    pub fn new(backend: B, network: Network, path: PathBuf) -> Result<Self> {
        let journal = Journal::load(&path)?;
        Ok(Self {
            backend,
            network,
            journal,
            path,
            leases: BTreeMap::new(),
        })
    }
    pub fn persist(&self) -> Result<()> {
        atomic_json(&self.path, &self.journal)
    }
    pub fn ensure_prefix(&mut self, prefix: &str, expires: u64) -> Result<()> {
        let c = Cidr::parse_public(prefix)?;
        let ip = Ipv4Addr::from(c.base);
        let owned = self
            .journal
            .routes
            .iter()
            .any(|r| r.destination == prefix && r.network == self.network);
        if owned && self.backend.is_local(ip, &self.network)? {
            self.leases
                .entry(prefix.into())
                .and_modify(|t| *t = (*t).max(expires))
                .or_insert(expires);
            return Ok(());
        }
        if self.backend.is_local(ip, &self.network)? && c.prefix == 32 {
            return Ok(());
        }
        let r = Route {
            destination: prefix.into(),
            network: self.network.clone(),
        };
        ensure!(
            !self.backend.exact_exists(&r)?,
            "存在其他程序的同前缀路由，拒绝覆盖: {prefix}"
        );
        ensure!(self.journal.routes.len() < 24000, "路由数量超过安全上限");
        // Write-ahead intent: an exact route did not exist before the exclusive add.
        self.journal.routes.push(r.clone());
        self.persist()?;
        if let Err(e) = self.backend.add(&r) {
            // A timed-out OS command may already have applied the route. Keep
            // the write-ahead record until cleanup has verified/remediated it.
            self.backend.remove_owned(&r)?;
            self.journal.routes.pop();
            self.persist()?;
            return Err(e);
        }
        if !self.backend.is_local(ip, &self.network)? {
            self.backend.remove_owned(&r)?;
            self.journal.routes.pop();
            self.persist()?;
            anyhow::bail!("真实路由未指向本地网络: {prefix}");
        }
        self.leases.insert(prefix.into(), expires);
        Ok(())
    }
    /// Only called after the complete response has passed validation.
    pub fn ensure_answers(&mut self, addresses: &[Ipv4Addr], ttl: u32) -> Result<()> {
        ensure!(!addresses.is_empty(), "DNS 没有返回 IPv4，不能报告直连成功");
        ensure!(
            addresses.iter().all(|ip| public_v4(*ip)),
            "DNS 返回了私网 / 保留 / Tailnet 地址"
        );
        // Preserve previous leases on ANY failure. New verified routes expire normally.
        for ip in addresses {
            self.ensure_prefix(&format!("{ip}/32"), now() + u64::from(ttl) + 30)?;
        }
        Ok(())
    }
    pub fn expire(&mut self) -> Result<()> {
        let expired: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, t)| **t < now())
            .map(|(p, _)| p.clone())
            .collect();
        for p in expired {
            if let Some(i) = self.journal.routes.iter().position(|r| r.destination == p) {
                self.backend.remove_owned(&self.journal.routes[i])?;
                self.journal.routes.remove(i);
                self.persist()?;
            }
            self.leases.remove(&p);
        }
        Ok(())
    }
    pub fn cleanup_routes(&mut self) -> Result<()> {
        while let Some(r) = self.journal.routes.last().cloned() {
            self.backend.remove_owned(&r)?;
            self.journal.routes.pop();
            self.persist()?;
        }
        Ok(())
    }
}
