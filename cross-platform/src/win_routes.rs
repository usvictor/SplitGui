//! Windows route hot path uses IP Helper directly. No PowerShell startup per DNS answer.
use crate::{config::Cidr, routes::Route};
use anyhow::{ensure, Result};
use std::{net::Ipv4Addr, ptr};
use windows_sys::Win32::{NetworkManagement::IpHelper::*, Networking::WinSock::*};

fn check(code: u32) -> Result<()> {
    ensure!(
        code == 0,
        "IP Helper error: {}",
        std::io::Error::from_raw_os_error(code as i32)
    );
    Ok(())
}
fn address(ip: Ipv4Addr) -> SOCKADDR_INET {
    let mut v = SOCKADDR_IN::default();
    v.sin_family = AF_INET;
    v.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets());
    SOCKADDR_INET { Ipv4: v }
}
fn ip(v: SOCKADDR_INET) -> Ipv4Addr {
    // Only called for rows from the AF_INET-filtered table.
    unsafe { Ipv4Addr::from(v.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()) }
}
fn table() -> Result<Vec<MIB_IPFORWARD_ROW2>> {
    let mut p = ptr::null_mut();
    unsafe {
        check(GetIpForwardTable2(AF_INET, &mut p))?;
        ensure!(!p.is_null(), "Empty IP Helper table pointer");
        let rows =
            std::slice::from_raw_parts((*p).Table.as_ptr(), (*p).NumEntries as usize).to_vec();
        FreeMibTable(p.cast());
        Ok(rows)
    }
}
pub fn interface(destination: Ipv4Addr) -> Result<String> {
    let mut row = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();
    unsafe {
        check(GetBestRoute2(
            ptr::null(),
            0,
            ptr::null(),
            &address(destination),
            0,
            &mut row,
            &mut source,
        ))?;
    }
    Ok(row.InterfaceIndex.to_string())
}
pub fn exists(route: &Route) -> Result<bool> {
    let c = Cidr::parse_public(&route.destination)?;
    Ok(table()?.iter().any(|r| {
        r.DestinationPrefix.PrefixLength == c.prefix
            && u32::from(ip(r.DestinationPrefix.Prefix)) == c.base
    }))
}
pub fn add(route: &Route) -> Result<()> {
    let c = Cidr::parse_public(&route.destination)?;
    let mut row = MIB_IPFORWARD_ROW2::default();
    unsafe {
        InitializeIpForwardEntry(&mut row);
    }
    row.InterfaceIndex = route.network.interface.parse()?;
    row.DestinationPrefix.Prefix = address(Ipv4Addr::from(c.base));
    row.DestinationPrefix.PrefixLength = c.prefix;
    row.NextHop = address(route.network.gateway);
    row.Metric = 3;
    row.Protocol = MIB_IPPROTO_NETMGMT;
    unsafe { check(CreateIpForwardEntry2(&row)) }
}
pub fn remove(route: &Route) -> Result<()> {
    let c = Cidr::parse_public(&route.destination)?;
    let index: u32 = route.network.interface.parse()?;
    for row in table()? {
        if row.InterfaceIndex == index
            && row.DestinationPrefix.PrefixLength == c.prefix
            && u32::from(ip(row.DestinationPrefix.Prefix)) == c.base
            && ip(row.NextHop) == route.network.gateway
            && row.Metric == 3
            && row.Protocol == MIB_IPPROTO_NETMGMT
        {
            unsafe {
                check(DeleteIpForwardEntry2(&row))?;
            }
        }
    }
    Ok(())
}
