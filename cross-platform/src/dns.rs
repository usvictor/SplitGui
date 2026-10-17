use crate::config::{public_v4, Config};
use anyhow::{bail, ensure, Context, Result};
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::{DNSClass, RData, RecordType};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    time::{Duration, Instant},
};

pub trait AnswerRoutes {
    fn prepare(&mut self, ips: &[Ipv4Addr], ttl: u32) -> Result<()>;
}
impl<B: crate::routes::RouteBackend> AnswerRoutes for crate::routes::Routes<B> {
    fn prepare(&mut self, ips: &[Ipv4Addr], ttl: u32) -> Result<()> {
        self.ensure_answers(ips, ttl)
    }
}

pub trait Upstream {
    fn exchange(&mut self, request: &Message, server: Ipv4Addr) -> Result<Message>;
}
pub struct NetworkUpstream;

impl Upstream for NetworkUpstream {
    fn exchange(&mut self, request: &Message, server: Ipv4Addr) -> Result<Message> {
        let timeout = Duration::from_millis(1200);
        let address = SocketAddr::from((server, 53));
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        socket.connect(address)?; // kernel checks the source IP and port
        let mut wire = request.clone();
        wire.set_id(rand::random());
        let bytes = wire.to_vec()?;
        socket.send(&bytes)?;
        let mut buf = [0u8; 65535];
        let n = socket.recv(&mut buf).context("国内 DNS 超时 / 无响应")?;
        let mut response = Message::from_vec(&buf[..n])?;
        validate_envelope(&wire, &response)?;
        if response.truncated() {
            let mut tcp = TcpStream::connect_timeout(&address, timeout)?;
            tcp.set_read_timeout(Some(timeout))?;
            tcp.set_write_timeout(Some(timeout))?;
            tcp.write_all(&(bytes.len() as u16).to_be_bytes())?;
            tcp.write_all(&bytes)?;
            let mut length = [0; 2];
            tcp.read_exact(&mut length)?;
            let mut bytes = vec![0; u16::from_be_bytes(length) as usize];
            tcp.read_exact(&mut bytes)?;
            response = Message::from_vec(&bytes)?;
            validate_envelope(&wire, &response)?;
            ensure!(!response.truncated(), "TCP DNS 仍然被截断");
        }
        response.set_id(request.id());
        Ok(response)
    }
}

pub fn validate_envelope(request: &Message, response: &Message) -> Result<()> {
    ensure!(
        response.message_type() == MessageType::Response
            && response.id() == request.id()
            && response.op_code() == request.op_code()
            && response.queries() == request.queries(),
        "DNS 响应 ID / 问题 / 操作码不匹配"
    );
    Ok(())
}

pub fn reply(request: &Message, code: ResponseCode) -> Message {
    let mut r = Message::new();
    r.set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_recursion_desired(request.recursion_desired())
        .set_recursion_available(true)
        .set_response_code(code)
        .add_queries(request.queries().iter().cloned());
    r
}

#[derive(Clone)]
struct Entry {
    response: Message,
    at: Instant,
    ttl: u32,
    ips: Vec<Ipv4Addr>,
}

pub struct Resolver<U: Upstream> {
    config: Config,
    upstream: U,
    cache: HashMap<String, Entry>,
    pub last_error: Option<String>,
    pub successful_answers: u64,
    pub failed_answers: u64,
}

impl<U: Upstream> Resolver<U> {
    pub fn new(config: Config, upstream: U) -> Self {
        Self {
            config,
            upstream,
            cache: HashMap::new(),
            last_error: None,
            successful_answers: 0,
            failed_answers: 0,
        }
    }
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
    pub fn handle<R: AnswerRoutes>(&mut self, packet: &[u8], routes: &mut R) -> Result<Vec<u8>> {
        let query = Message::from_vec(packet)?;
        match self.resolve(&query, routes) {
            Ok(r) => Ok(r.to_vec()?),
            Err(e) => {
                self.failed_answers += 1;
                self.last_error = Some(format!("{e:#}"));
                Ok(reply(&query, ResponseCode::ServFail).to_vec()?)
            }
        }
    }
    fn resolve<R: AnswerRoutes>(&mut self, query: &Message, routes: &mut R) -> Result<Message> {
        ensure!(
            query.message_type() == MessageType::Query
                && query.op_code() == OpCode::Query
                && query.queries().len() == 1,
            "无效 DNS 查询"
        );
        let q = &query.queries()[0];
        let name = q
            .name()
            .to_ascii()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if q.query_class() != DNSClass::IN || !self.config.domestic(&name) {
            // Never forward unknown/LAN/overseas names to domestic resolvers.
            return Ok(reply(query, ResponseCode::Refused));
        }
        match q.query_type() {
            // v2 alpha explicitly uses IPv4 for managed domains. Do not leak an
            // unverified IPv6 address via AAAA or HTTPS/SVCB hints.
            RecordType::AAAA | RecordType::HTTPS | RecordType::SVCB => {
                return Ok(reply(query, ResponseCode::NoError))
            }
            RecordType::A => {}
            _ => return Ok(reply(query, ResponseCode::Refused)),
        }
        if let Some(e) = self.cache.get(&name) {
            let age = e.at.elapsed().as_secs() as u32;
            if age < e.ttl {
                routes.prepare(&e.ips, e.ttl - age)?; // verify even on cache hit
                let mut r = e.response.clone();
                r.set_id(query.id());
                for a in r.answers_mut() {
                    a.set_ttl(a.ttl().saturating_sub(age));
                }
                self.successful_answers += 1;
                return Ok(r);
            }
        }
        self.cache.remove(&name);
        let mut failures = vec![];
        for server in self.config.domestic_dns.clone() {
            // A configured domestic resolver must itself use the local route.
            // Recheck on every upstream query, not just during startup.
            routes.prepare(&[server], 60)?;
            let result = self.upstream.exchange(query, server).and_then(|response| {
                validate_envelope(query, &response)?;
                let (r, ips, ttl) = validated_answer(query, &response)?;
                routes.prepare(&ips, ttl)?;
                Ok((r, ips, ttl))
            });
            match result {
                Ok((r, ips, ttl)) => {
                    if self.cache.len() >= 2048 {
                        self.cache.clear();
                    }
                    if ttl > 0 {
                        self.cache.insert(
                            name,
                            Entry {
                                response: r.clone(),
                                at: Instant::now(),
                                ttl,
                                ips,
                            },
                        );
                    }
                    self.successful_answers += 1;
                    // Historical failure is retained for diagnostics until a new service session.
                    return Ok(r);
                }
                Err(e) => failures.push(format!("{server}: {e:#}")),
            }
        }
        bail!(
            "{}；没有回退到海外 / 系统 DNS，也没有修改出口节点",
            failures.join("; ")
        )
    }
}

pub fn validated_answer(
    query: &Message,
    response: &Message,
) -> Result<(Message, Vec<Ipv4Addr>, u32)> {
    ensure!(
        response.response_code() == ResponseCode::NoError,
        "DNS 返回 {}，不是直连成功",
        response.response_code()
    );
    ensure!(!response.truncated(), "截断响应不能用于创建路由");
    let mut current = query.queries()[0].name().clone();
    let mut seen = HashSet::new();
    let mut selected = vec![];
    let mut ips = vec![];
    for _ in 0..16 {
        ensure!(seen.insert(current.clone()), "CNAME 循环");
        let records: Vec<_> = response
            .answers()
            .iter()
            .filter(|a| a.name() == &current)
            .collect();
        let aliases: Vec<_> = records
            .iter()
            .filter_map(|a| match a.data() {
                RData::CNAME(n) => Some((a, n)),
                _ => None,
            })
            .collect();
        ensure!(aliases.len() <= 1, "同一名称存在多个 CNAME");
        if let Some((a, n)) = aliases.first() {
            ensure!(
                !records.iter().any(|r| r.record_type() == RecordType::A),
                "CNAME 和 A 冲突"
            );
            // CNAME target is not independently queried through another policy.
            // Require the complete chain in this same domestic response.
            ensure!(
                !crate::config::protected_name(n.0.to_ascii().trim_end_matches('.')),
                "CNAME 指向保护域名"
            );
            selected.push((***a).clone());
            current = n.0.clone();
            continue;
        }
        for a in records {
            if let RData::A(ip) = a.data() {
                ensure!(
                    a.dns_class() == DNSClass::IN && public_v4(ip.0),
                    "异常 DNS 地址: {}",
                    ip.0
                );
                selected.push(a.clone());
                ips.push(ip.0);
            }
        }
        break;
    }
    ensure!(!ips.is_empty(), "DNS 响应为空 / CNAME 链不完整");
    let ttl = selected.iter().map(|a| a.ttl()).min().unwrap_or(0).min(60);
    let mut r = reply(query, ResponseCode::NoError);
    for mut a in selected {
        a.set_ttl(ttl);
        r.add_answer(a);
    }
    // New response deliberately carries no AD bit, RRSIG or unverified additional records.
    Ok((r, ips, ttl))
}
