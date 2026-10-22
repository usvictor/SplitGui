use anyhow::{bail, Result};
use hickory_proto::{
    op::{Message, Query, ResponseCode},
    rr::{
        rdata::{A, CNAME},
        Name, RData, Record, RecordType,
    },
};
use std::{cell::Cell, net::Ipv4Addr, rc::Rc};
use tailscale_splitgui::{
    config::{normalize_domain, public_v4, Cidr, Config},
    dns::{self, AnswerRoutes, Resolver, Upstream},
    platform,
    routes::{Journal, Network, Route, RouteBackend, Routes},
    runtime,
};

fn config() -> Config {
    let mut c = Config {
        domestic_domains: vec!["baidu.com".into()],
        ..Config::default()
    };
    c.validate().unwrap();
    c
}
fn query(name: &str, kind: RecordType) -> Message {
    let mut q = Message::new();
    q.set_id(42)
        .set_recursion_desired(true)
        .add_query(Query::query(
            Name::from_ascii(format!("{}.", name.trim_end_matches('.'))).unwrap(),
            kind,
        ));
    q
}
fn answer(q: &Message, ip: &str, ttl: u32) -> Message {
    let mut r = dns::reply(q, ResponseCode::NoError);
    r.add_answer(Record::from_rdata(
        q.queries()[0].name().clone(),
        ttl,
        RData::A(A(ip.parse().unwrap())),
    ));
    r
}
struct FakeUpstream {
    calls: Rc<Cell<usize>>,
    response: Message,
    fail: bool,
}
impl Upstream for FakeUpstream {
    fn exchange(&mut self, q: &Message, _: Ipv4Addr) -> Result<Message> {
        self.calls.set(self.calls.get() + 1);
        if self.fail {
            bail!("timeout")
        };
        let mut r = self.response.clone();
        r.set_id(q.id());
        Ok(r)
    }
}
#[derive(Default)]
struct Prepared {
    calls: Vec<Vec<Ipv4Addr>>,
    fail: bool,
}
impl AnswerRoutes for Prepared {
    fn prepare(&mut self, ips: &[Ipv4Addr], _: u32) -> Result<()> {
        if self.fail {
            bail!("verification failed")
        };
        self.calls.push(ips.to_vec());
        Ok(())
    }
}
fn resolver(response: Message, fail: bool) -> (Resolver<FakeUpstream>, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    (
        Resolver::new(
            config(),
            FakeUpstream {
                calls: calls.clone(),
                response,
                fail,
            },
        ),
        calls,
    )
}
fn resolve(r: &mut Resolver<FakeUpstream>, q: &Message, p: &mut Prepared) -> Message {
    Message::from_vec(&r.handle(&q.to_vec().unwrap(), p).unwrap()).unwrap()
}

#[test]
fn suffix_boundary() {
    let c = config();
    assert!(c.domestic("a.baidu.com."));
    assert!(c.domestic("BAIDU.COM"));
    assert!(!c.domestic("evilbaidu.com"));
    assert!(!c.domestic("baidu.com.evil.net"));
}
#[test]
fn protected_names_rejected() {
    for d in [
        "printer.local",
        "server.ts.net",
        "home.arpa",
        "1.168.192.in-addr.arpa",
        "a.invalid",
    ] {
        let mut c = config();
        c.domestic_domains = vec![d.into()];
        assert!(c.validate().is_err(), "{d}");
    }
}
#[test]
fn dangerous_names_rejected() {
    for d in [
        "a.com';Remove-Item x",
        "../x",
        "https://baidu.com",
        "*.com",
        ".com",
        "127.0.0.1",
        "bad..com",
        "a-.com",
    ] {
        assert!(normalize_domain(d).is_err(), "{d}");
    }
}
#[test]
fn normalized_domain() {
    assert_eq!(
        normalize_domain(" WWW.BAIDU.COM. ").unwrap(),
        "www.baidu.com"
    );
}
#[test]
fn protected_networks() {
    for ip in [
        "127.0.0.1",
        "100.64.0.1",
        "192.168.1.1",
        "198.18.0.1",
        "224.0.0.1",
        "203.0.113.1",
        "0.0.0.0",
    ] {
        assert!(!public_v4(ip.parse().unwrap()));
    }
    assert!(public_v4("223.5.5.5".parse().unwrap()));
}
#[test]
fn cidr_rejects_broad_and_unaligned() {
    for p in [
        "0.0.0.0/0",
        "0.0.0.0/1",
        "100.0.0.0/8",
        "10.0.0.0/8",
        "8.8.8.9/24",
        "8.0.0.0/33",
    ] {
        assert!(Cidr::parse_public(p).is_err(), "{p}");
    }
    assert!(Cidr::parse_public("8.8.8.0/24").is_ok());
}
#[test]
fn unknown_and_lan_never_leak() {
    for n in [
        "google.com",
        "printer.local",
        "nas.home.arpa",
        "host.ts.net",
    ] {
        let q = query(n, RecordType::A);
        let (mut r, calls) = resolver(answer(&q, "8.8.8.8", 60), false);
        let mut p = Prepared::default();
        assert_eq!(
            resolve(&mut r, &q, &mut p).response_code(),
            ResponseCode::Refused
        );
        assert_eq!(calls.get(), 0);
        assert!(p.calls.is_empty());
    }
}
#[test]
fn timeout_does_not_succeed_or_fallback() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, calls) = resolver(answer(&q, "8.8.8.8", 60), true);
    let response = resolve(&mut r, &q, &mut Prepared::default());
    assert_eq!(response.response_code(), ResponseCode::ServFail);
    assert_eq!(calls.get(), 2);
    assert_eq!(r.successful_answers, 0);
    assert_eq!(r.failed_answers, 1);
}
#[test]
fn empty_response_not_success() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, _) = resolver(dns::reply(&q, ResponseCode::NoError), false);
    let mut p = Prepared::default();
    assert_eq!(
        resolve(&mut r, &q, &mut p).response_code(),
        ResponseCode::ServFail
    );
    assert_eq!(r.successful_answers, 0);
    assert!(p
        .calls
        .iter()
        .all(|v| v == &vec!["223.5.5.5".parse::<Ipv4Addr>().unwrap()]
            || v == &vec!["119.29.29.29".parse::<Ipv4Addr>().unwrap()]));
}
#[test]
fn nxdomain_not_success() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, _) = resolver(dns::reply(&q, ResponseCode::NXDomain), false);
    assert_eq!(
        resolve(&mut r, &q, &mut Prepared::default()).response_code(),
        ResponseCode::ServFail
    );
    assert_eq!(r.successful_answers, 0);
}
#[test]
fn private_address_rejected() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, _) = resolver(answer(&q, "192.168.1.1", 60), false);
    let mut p = Prepared::default();
    assert_eq!(
        resolve(&mut r, &q, &mut p).response_code(),
        ResponseCode::ServFail
    );
    assert!(!p
        .calls
        .iter()
        .flatten()
        .any(|i| *i == "192.168.1.1".parse::<Ipv4Addr>().unwrap()));
}
#[test]
fn mixed_answer_rejected_before_routes() {
    let q = query("baidu.com", RecordType::A);
    let mut a = answer(&q, "8.8.8.8", 60);
    a.add_answer(Record::from_rdata(
        q.queries()[0].name().clone(),
        60,
        RData::A(A("10.1.1.1".parse().unwrap())),
    ));
    assert!(dns::validated_answer(&q, &a).is_err());
}
#[test]
fn route_failure_returns_servfail() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, _) = resolver(answer(&q, "8.8.8.8", 60), false);
    assert_eq!(
        resolve(
            &mut r,
            &q,
            &mut Prepared {
                fail: true,
                ..Default::default()
            }
        )
        .response_code(),
        ResponseCode::ServFail
    );
    assert_eq!(r.successful_answers, 0);
}
#[test]
fn cache_hit_still_validates_route() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, calls) = resolver(answer(&q, "8.8.8.8", 60), false);
    let mut p = Prepared::default();
    assert_eq!(
        resolve(&mut r, &q, &mut p).response_code(),
        ResponseCode::NoError
    );
    p.fail = true;
    assert_eq!(
        resolve(&mut r, &q, &mut p).response_code(),
        ResponseCode::ServFail
    );
    assert_eq!(calls.get(), 1);
}
#[test]
fn zero_ttl_is_not_cached() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, calls) = resolver(answer(&q, "8.8.8.8", 0), false);
    for _ in 0..2 {
        resolve(&mut r, &q, &mut Prepared::default());
    }
    assert_eq!(calls.get(), 2);
}
#[test]
fn cache_flush_after_network_change() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, calls) = resolver(answer(&q, "8.8.8.8", 60), false);
    resolve(&mut r, &q, &mut Prepared::default());
    r.clear_cache();
    resolve(&mut r, &q, &mut Prepared::default());
    assert_eq!(calls.get(), 2);
}
#[test]
fn ttl_never_extended() {
    let q = query("baidu.com", RecordType::A);
    for ttl in [0, 1, 10, 60, 600] {
        let (r, _, t) = dns::validated_answer(&q, &answer(&q, "8.8.8.8", ttl)).unwrap();
        assert_eq!(t, ttl.min(60));
        assert_eq!(r.answers()[0].ttl(), ttl.min(60));
    }
}
#[test]
fn ipv6_and_hints_cannot_bypass() {
    for kind in [RecordType::AAAA, RecordType::HTTPS, RecordType::SVCB] {
        let q = query("baidu.com", kind);
        let (mut r, calls) = resolver(dns::reply(&q, ResponseCode::NoError), false);
        let result = resolve(&mut r, &q, &mut Prepared::default());
        assert!(result.answers().is_empty());
        assert_eq!(calls.get(), 0);
    }
}
#[test]
fn bad_response_id_rejected() {
    let q = query("baidu.com", RecordType::A);
    let mut r = answer(&q, "8.8.8.8", 60);
    r.set_id(99);
    assert!(dns::validate_envelope(&q, &r).is_err());
}
#[test]
fn wrong_question_rejected() {
    let q = query("baidu.com", RecordType::A);
    let r = answer(&query("google.com", RecordType::A), "8.8.8.8", 60);
    assert!(dns::validate_envelope(&q, &r).is_err());
}
#[test]
fn cname_chain_min_ttl() {
    let q = query("baidu.com", RecordType::A);
    let mut r = dns::reply(&q, ResponseCode::NoError);
    r.add_answer(Record::from_rdata(
        q.queries()[0].name().clone(),
        10,
        RData::CNAME(CNAME(Name::from_ascii("cdn.baidu.com.").unwrap())),
    ));
    r.add_answer(Record::from_rdata(
        Name::from_ascii("cdn.baidu.com.").unwrap(),
        300,
        RData::A(A("8.8.8.8".parse().unwrap())),
    ));
    let (result, ips, ttl) = dns::validated_answer(&q, &r).unwrap();
    assert_eq!(ips.len(), 1);
    assert_eq!(ttl, 10);
    assert!(result.answers().iter().all(|a| a.ttl() == 10));
}
#[test]
fn cname_loop_rejected() {
    let q = query("baidu.com", RecordType::A);
    let mut r = dns::reply(&q, ResponseCode::NoError);
    r.add_answer(Record::from_rdata(
        q.queries()[0].name().clone(),
        10,
        RData::CNAME(CNAME(q.queries()[0].name().clone())),
    ));
    assert!(dns::validated_answer(&q, &r).is_err());
}
#[test]
fn unrelated_additional_a_not_routed() {
    let q = query("baidu.com", RecordType::A);
    let mut r = answer(&q, "8.8.8.8", 60);
    r.add_additional(Record::from_rdata(
        Name::from_ascii("other.com").unwrap(),
        60,
        RData::A(A("10.0.0.1".parse().unwrap())),
    ));
    let (r, ips, _) = dns::validated_answer(&q, &r).unwrap();
    assert_eq!(ips.len(), 1);
    assert!(r.additionals().is_empty());
    assert!(!r.authentic_data());
}
#[test]
fn truncated_reply_cannot_create_routes() {
    let q = query("baidu.com", RecordType::A);
    let mut r = answer(&q, "8.8.8.8", 60);
    r.set_truncated(true);
    assert!(dns::validated_answer(&q, &r).is_err());
}
#[test]
fn malformed_packets_do_not_panic() {
    let q = query("baidu.com", RecordType::A);
    let (mut r, _) = resolver(answer(&q, "8.8.8.8", 60), false);
    for n in 0..256 {
        let bytes: Vec<u8> = (0..n).map(|i| ((i * 31 + n) % 256) as u8).collect();
        let _ = r.handle(&bytes, &mut Prepared::default());
    }
}
#[test]
fn shell_quotes_preserve_literal_arguments() {
    assert_eq!(platform::ps_quote("a'b"), "'a''b'");
    assert_eq!(platform::sh_quote("a'b"), "'a'\\''b'");
}
#[test]
fn oversized_udp_is_truncated() {
    let q = query("baidu.com", RecordType::A);
    let mut r = answer(&q, "8.8.8.8", 60);
    for _ in 0..50 {
        r.add_answer(r.answers()[0].clone());
    }
    let wire = runtime::udp_response(&r.to_vec().unwrap()).unwrap();
    let out = Message::from_vec(&wire).unwrap();
    assert!(wire.len() <= 512);
    assert!(out.truncated());
    assert_eq!(out.queries(), q.queries());
}

#[derive(Default)]
struct FakeBackend {
    added: Vec<Route>,
    removed: Vec<Route>,
    conflict: bool,
    verify_fail: bool,
    ambiguous_add_failure: bool,
    cleanup_failure: bool,
}
impl RouteBackend for FakeBackend {
    fn is_local(&mut self, _: Ipv4Addr, _: &Network) -> Result<bool> {
        Ok(!self.added.is_empty() && !self.verify_fail)
    }
    fn exact_exists(&mut self, _: &Route) -> Result<bool> {
        Ok(self.conflict)
    }
    fn add(&mut self, r: &Route) -> Result<()> {
        self.added.push(r.clone());
        if self.ambiguous_add_failure {
            bail!("command timed out after applying route");
        }
        Ok(())
    }
    fn remove_owned(&mut self, r: &Route) -> Result<()> {
        if self.cleanup_failure {
            bail!("cleanup temporarily failed");
        }
        self.removed.push(r.clone());
        self.added.retain(|a| a != r);
        Ok(())
    }
}
fn engine(b: FakeBackend) -> Routes<FakeBackend> {
    let dir = std::env::temp_dir().join(format!(
        "splitgui-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Routes::new(
        b,
        Network {
            interface: "en0".into(),
            gateway: "192.168.1.1".parse().unwrap(),
            address: "192.168.1.2".parse().unwrap(),
        },
        dir.join("journal.json"),
    )
    .unwrap()
}
#[test]
fn no_default_route_can_be_created() {
    let mut e = engine(FakeBackend::default());
    assert!(e.ensure_prefix("0.0.0.0/0", u64::MAX).is_err());
    assert!(e.backend.added.is_empty());
}
#[test]
fn existing_route_not_overwritten() {
    let mut e = engine(FakeBackend {
        conflict: true,
        ..Default::default()
    });
    assert!(e.ensure_prefix("8.8.8.8/32", u64::MAX).is_err());
    assert!(e.backend.removed.is_empty());
    assert!(e.backend.added.is_empty());
}
#[test]
fn verification_failure_rolls_back_new_route() {
    let mut e = engine(FakeBackend {
        verify_fail: true,
        ..Default::default()
    });
    assert!(e.ensure_prefix("8.8.8.8/32", u64::MAX).is_err());
    assert_eq!(e.backend.removed.len(), 1);
    assert!(e.journal.routes.is_empty());
}
#[test]
fn cache_refresh_does_not_delete_own_route() {
    let mut e = engine(FakeBackend::default());
    e.ensure_answers(&["8.8.8.8".parse().unwrap()], 60).unwrap();
    e.ensure_answers(&["8.8.8.8".parse().unwrap()], 60).unwrap();
    assert_eq!(e.backend.added.len(), 1);
    assert!(e.backend.removed.is_empty());
}
#[test]
fn empty_refresh_preserves_known_good_route() {
    let mut e = engine(FakeBackend::default());
    e.ensure_answers(&["8.8.8.8".parse().unwrap()], 60).unwrap();
    assert!(e.ensure_answers(&[], 60).is_err());
    assert_eq!(e.journal.routes.len(), 1);
    assert!(e.backend.removed.is_empty());
}
#[test]
fn journal_recovers_owned_routes() {
    let mut e = engine(FakeBackend::default());
    e.ensure_answers(&["8.8.8.8".parse().unwrap()], 60).unwrap();
    assert_eq!(Journal::load(&e.path).unwrap().routes.len(), 1);
    e.cleanup_routes().unwrap();
    assert!(Journal::load(&e.path).unwrap().routes.is_empty());
}

#[test]
fn timed_out_add_cleans_applied_side_effect() {
    let mut e = engine(FakeBackend {
        ambiguous_add_failure: true,
        ..Default::default()
    });
    assert!(e.ensure_prefix("8.8.8.8/32", u64::MAX).is_err());
    assert!(e.backend.added.is_empty());
    assert!(Journal::load(&e.path).unwrap().routes.is_empty());
}

#[test]
fn failed_cleanup_keeps_recovery_journal() {
    let mut e = engine(FakeBackend {
        ambiguous_add_failure: true,
        cleanup_failure: true,
        ..Default::default()
    });
    assert!(e.ensure_prefix("8.8.8.8/32", u64::MAX).is_err());
    assert_eq!(Journal::load(&e.path).unwrap().routes.len(), 1);
    e.backend.cleanup_failure = false;
    e.cleanup_routes().unwrap();
    assert!(e.backend.added.is_empty());
}

#[test]
fn mac_route_table_shorthand_matches_without_broadening() {
    for s in ["8.8.8/24", "8.8.8.0/24", "8.8.8"] {
        assert!(platform::mac_destination_matches(s, "8.8.8.0/24"));
    }
    assert!(!platform::mac_destination_matches("8.8.8.8", "8.8.8.0/24"));
    assert!(!platform::mac_destination_matches("default", "0.0.0.0/0"));
}

#[test]
fn admin_report_preserves_chinese_and_rejects_stale_result() {
    let path = std::env::temp_dir().join(format!("splitgui-admin-{}.json", rand::random::<u64>()));
    std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"request":"current", "error":"检查本地网卡失败：请先允许局域网访问"})).unwrap()).unwrap();
    assert_eq!(
        tailscale_splitgui::service::admin_report(&path, "current")
            .unwrap()
            .as_deref(),
        Some("检查本地网卡失败：请先允许局域网访问")
    );
    assert!(tailscale_splitgui::service::admin_report(&path, "previous").is_err());
    std::fs::remove_file(path).unwrap();
}

#[cfg(windows)]
#[test]
fn powershell_chinese_output_and_errors_are_utf8() {
    assert_eq!(
        platform::powershell("Write-Output '中文网卡：以太网'").unwrap(),
        "中文网卡：以太网"
    );
    let error = platform::powershell("throw '请开启允许局域网访问'")
        .unwrap_err()
        .to_string();
    assert!(error.contains("请开启允许局域网访问"), "{error}");
    assert!(!error.contains('\u{fffd}'), "{error}");
}
