mod host_addr;

pub const CERT_KEY: &[u8] = include_bytes!("key.pem");
pub const CERT_CRT: &[u8] = include_bytes!("crt.pem");

pub use host_addr::select_host_ip;
