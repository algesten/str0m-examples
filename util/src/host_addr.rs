use anyhow::{bail, Result};
use std::net::IpAddr;
use systemstat::{Platform, System};

pub fn select_host_ip() -> Result<IpAddr> {
    let system = System::new();
    let networks = system.networks().unwrap();

    for net in networks.values() {
        for n in &net.addrs {
            if let systemstat::IpAddr::V4(v) = n.addr {
                if !v.is_loopback() && !v.is_link_local() && !v.is_broadcast() {
                    return Ok(IpAddr::V4(v));
                }
            }
        }
    }

    bail!("Found no usable network interface");
}
