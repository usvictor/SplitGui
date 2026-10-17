use crate::{
    config::Config,
    dns::{NetworkUpstream, Resolver},
    platform::{self, NativeRoutes},
    routes::{atomic_json, now, Journal, Network, Routes},
};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub message: String,
    pub updated: u64,
    pub exit_node: Option<String>,
    pub network: Option<Network>,
    pub answers: u64,
    pub failures: u64,
    pub last_dns_error: Option<String>,
}
impl Status {
    pub fn new(state: &str, message: &str) -> Self {
        Self {
            state: state.into(),
            message: message.into(),
            updated: now(),
            exit_node: None,
            network: None,
            answers: 0,
            failures: 0,
            last_dns_error: None,
        }
    }
    pub fn save(&mut self) -> Result<()> {
        self.updated = now();
        atomic_json(&platform::data_dir().join("status.json"), self)
    }
}

/// Called before bind/apply and by the independent supervisor after worker exit.
/// DNS is removed FIRST; then owned routes, and finally the journal.
pub fn recover() -> Result<()> {
    let path = platform::data_dir().join("journal.json");
    let mut journal = Journal::load(&path)?;
    for (domain, identity) in journal.dns.clone() {
        platform::remove_dns(&domain, &identity)?;
        journal.dns.remove(&domain);
        atomic_json(&path, &journal)?;
    }
    platform::flush_dns()?;
    use crate::routes::RouteBackend;
    while let Some(route) = journal.routes.last().cloned() {
        NativeRoutes.remove_owned(&route)?;
        journal.routes.pop();
        atomic_json(&path, &journal)?;
    }
    Ok(())
}

pub fn worker() -> Result<()> {
    let dir = platform::data_dir();
    let stop = dir.join("stop");
    // OS service manager + supervisor guarantees a single worker.
    recover()?;
    if stop.exists() {
        return Ok(());
    }
    let mut config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    config.validate()?;
    let mut status = Status::new("starting", "正在验证出口、局域网、DNS 和真实路由");
    status.save()?;
    let exit = platform::exit_node()?;
    let network = platform::select_network(&config)?;
    ensure!(
        platform::route_interface(network.gateway)? == network.interface,
        "当前局域网网关未走物理网卡；未修改 DNS 或出口"
    );
    let overseas = platform::overseas_route()?;
    platform::preflight_dns(&config.domestic_domains)?;
    // Bind BOTH transports before making any scoped DNS changes.
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 53))?;
    let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 53))?;
    udp.set_nonblocking(true)?;
    tcp.set_nonblocking(true)?;
    let mut routes = Routes::new(NativeRoutes, network.clone(), dir.join("journal.json"))?;
    for ip in &config.domestic_dns {
        routes.ensure_prefix(&format!("{ip}/32"), u64::MAX)?;
    }
    for prefix in &config.china_routes {
        if stop.exists() {
            bail!("启用过程已取消");
        }
        routes.ensure_prefix(prefix, u64::MAX)?;
    }
    let mut resolver = Resolver::new(config.clone(), NetworkUpstream);
    // Full domestic query + route verification BEFORE system policy activation.
    let probe = make_query(&config.domestic_domains[0])?;
    let bytes = resolver.handle(&probe, &mut routes)?;
    let response = hickory_proto::op::Message::from_vec(&bytes)?;
    ensure!(
        response.response_code() == hickory_proto::op::ResponseCode::NoError
            && !response.answers().is_empty(),
        "国内 DNS 预检失败: {:?}",
        resolver.last_error
    );
    for domain in &config.domestic_domains {
        if stop.exists() {
            bail!("启用过程已取消");
        }
        routes.journal.dns.insert(domain.clone(), "pending".into());
        routes.persist()?;
        let identity = platform::install_dns(domain)?;
        routes.journal.dns.insert(domain.clone(), identity);
        routes.persist()?;
    }
    platform::flush_dns()?;
    ensure!(
        platform::exit_node()? == exit && platform::overseas_route()? == overseas,
        "出口状态发生变化；正在撤销本程序的变更"
    );
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    ctrlc::set_handler(move || {
        flag.store(false, Ordering::SeqCst);
    })?;
    status.state = "awaiting-system-check".into();
    status.message = "DNS 服务已就绪；正在检查系统解析是否实际命中规则".into();
    status.exit_node = Some(exit.clone());
    status.network = Some(network.clone());
    status.save()?;
    // System resolver runs in a separate process to bound getaddrinfo time. DNS
    // listener remains responsive while the probe blocks waiting for its answer.
    let executable = std::env::current_exe()?;
    let domain = config.domestic_domains[0].clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let r = platform::run(&executable, &["probe", &domain]);
        let _ = tx.send(r.map_err(|e| e.to_string()));
    });
    let mut system_checked = false;
    let mut probe_done = false;
    let mut last_check = Instant::now();
    let mut health: Option<std::sync::mpsc::Receiver<Result<(), String>>> = None;
    let mut last_status = Instant::now();
    let mut buf = [0u8; 65535];
    while running.load(Ordering::SeqCst) && !stop.exists() {
        match udp.recv_from(&mut buf) {
            Ok((n, peer)) if peer.ip().is_loopback() => {
                if let Ok(response) = resolver.handle(&buf[..n], &mut routes) {
                    // Classic UDP limit; clients retry TCP when TC is set.
                    let response = udp_response(&response)?;
                    let _ = udp.send_to(&response, peer);
                }
            }
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => return Err(e.into()),
            _ => {}
        }
        match tcp.accept() {
            Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                let _ = serve_tcp(&mut stream, &mut resolver, &mut routes);
            }
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => return Err(e.into()),
            _ => {}
        }
        if !probe_done {
            if let Ok(result) = rx.try_recv() {
                probe_done = true;
                let query_seen = resolver.successful_answers > 1;
                match result {
                    Ok(ips) if query_seen => {
                        let addresses = ips
                            .split_whitespace()
                            .map(str::parse::<Ipv4Addr>)
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        routes.ensure_answers(&addresses, 60)?;
                        system_checked = true;
                        status.state = "active".into();
                        status.message =
                            "系统解析已命中国内 DNS，返回地址的本地路由已验证；海外出口保持原状"
                                .into();
                    }
                    other => bail!(
                        "系统解析未通过验证（query_seen={query_seen}，{other:?}），不会报告成功"
                    ),
                }
            }
        }
        if let Some(rx) = &health {
            if let Ok(result) = rx.try_recv() {
                result.map_err(anyhow::Error::msg)?;
                health = None;
            }
        }
        if last_check.elapsed() > Duration::from_secs(5) && health.is_none() {
            // Network changes terminate the session; supervisor removes scoped DNS
            // first and owned routes second. Never rewrite the selected exit node.
            let c = config.clone();
            let n = network.clone();
            let e = exit.clone();
            let o = overseas.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            health = Some(rx);
            thread::spawn(move || {
                let result = (|| -> Result<()> {
                    ensure!(
                        platform::select_network(&c)? == n,
                        "物理网络已变化；请重新启用"
                    );
                    ensure!(
                        platform::exit_node()? == e && platform::overseas_route()? == o,
                        "出口 / Tailscale 状态改变；保留用户设置，撤销本程序规则"
                    );
                    Ok(())
                })();
                let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            });
            routes.expire()?;
            last_check = Instant::now();
        }
        if last_status.elapsed() > Duration::from_secs(2) {
            status.answers = resolver.successful_answers;
            status.failures = resolver.failed_answers;
            status.last_dns_error = resolver.last_error.clone();
            if resolver.failed_answers > 0 && system_checked {
                status.state = "degraded".into();
                status.message =
                    "部分国内 DNS 请求失败；未切换出口，历史错误见下方。重新启用可重新验收".into();
            }
            status.save()?;
            last_status = Instant::now();
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub fn make_query(domain: &str) -> Result<Vec<u8>> {
    use hickory_proto::{
        op::{Message, Query},
        rr::{Name, RecordType},
    };
    let mut q = Message::new();
    q.set_id(rand::random())
        .set_recursion_desired(true)
        .add_query(Query::query(Name::from_ascii(domain)?, RecordType::A));
    Ok(q.to_vec()?)
}

fn serve_tcp(
    stream: &mut TcpStream,
    resolver: &mut Resolver<NetworkUpstream>,
    routes: &mut Routes<NativeRoutes>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut len = [0; 2];
    stream.read_exact(&mut len)?;
    let mut b = vec![0; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut b)?;
    let result = resolver.handle(&b, routes)?;
    stream.write_all(&(result.len() as u16).to_be_bytes())?;
    stream.write_all(&result)?;
    Ok(())
}

pub fn udp_response(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() <= 512 {
        return Ok(bytes.to_vec());
    }
    let r = hickory_proto::op::Message::from_vec(bytes)?;
    let mut truncated = crate::dns::reply(&r, r.response_code());
    truncated.set_truncated(true);
    Ok(truncated.to_vec()?)
}

pub fn system_probe(domain: &str) -> Result<()> {
    use std::net::ToSocketAddrs;
    let d = crate::config::normalize_domain(domain)?;
    let ips: std::collections::BTreeSet<_> = (d.as_str(), 443)
        .to_socket_addrs()?
        .filter_map(|s| match s.ip() {
            std::net::IpAddr::V4(a) => Some(a),
            _ => None,
        })
        .collect();
    ensure!(!ips.is_empty(), "系统解析没有 IPv4");
    for ip in ips {
        println!("{ip}");
    }
    Ok(())
}

pub fn read_status() -> Result<Status> {
    let mut s: Status =
        serde_json::from_slice(&std::fs::read(platform::data_dir().join("status.json"))?)?;
    if now().saturating_sub(s.updated) > 30 && !["stopped", "error"].contains(&s.state.as_str()) {
        s.state = "unknown".into();
        s.message = "后台心跳已过期；不能确认分流成功，请停止恢复后再启用".into();
    }
    Ok(s)
}

pub fn write_error(error: &str) {
    let _ = Status::new("error", error).save();
}

pub fn read_config(path: &Path) -> Result<Config> {
    ensure!(std::fs::metadata(path)?.len() <= 2_000_000, "配置文件过大");
    let mut c: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    c.validate()?;
    Ok(c)
}
