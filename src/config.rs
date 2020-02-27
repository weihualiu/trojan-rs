use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use clap::Clap;
use crypto::digest::Digest;
use crypto::sha2::Sha224;
use trust_dns_resolver::Resolver;

pub struct DnsEntry {
    pub address: IpAddr,
    pub expired_time: Instant,
}

#[derive(Clap)]
#[clap(version = "0.1", author = "Hoping White", about = "a trojan implementation using rust")]
pub struct Opts {
    #[clap(short = "c", long = "cert", help = "certificate file path")]
    pub cert: Option<String>,
    #[clap(short = "k", long = "key", help = "private key file path")]
    pub key: Option<String>,
    #[clap(short = "l", long = "log-file", help = "log file path")]
    pub log_file: Option<String>,
    #[clap(short = "a", long = "local-addr", default_value = "0.0.0.0:443", help = "listen address for server")]
    pub local_addr: String,
    #[clap(short = "A", long = "remote-addr", default_value = "127.0.0.1:80", help = "http backend server address")]
    pub remote_addr: String,
    #[clap(required = true, short = "p", long = "password", help = "passwords for negotiation")]
    password: Vec<String>,
    #[clap(short = "L", long = "log-level", default_value = "2", help = "log level, 0 for trace, 1 for debug, 2 for info, 3 for warning, 4 for error, 5 for off")]
    pub log_level: u8,
    #[clap(short = "d", long = "dns-cache-time", default_value = "300", help = "time in seconds for dns query cache")]
    dns_cache_time: u64,
    #[clap(short = "m", long = "marker", default_value = "255", help = "set marker used by tproxy")]
    pub marker: u8,
    #[clap(short = "M", long = "mode", default_value = "server", help = "program mode, valid options are server and proxy")]
    pub mode: String,
    #[clap(short = "h", long = "hostname", help = "trojan server hostname")]
    pub hostname: Option<String>,
    #[clap(short = "i", long = "idle-timeout", default_value = "300", help = "time in seconds before closing an inactive connection")]
    pub idle_timeout: u64,
    #[clap(skip)]
    dns_cache_duration: Duration,
    #[clap(skip)]
    sha_pass: Vec<String>,
    #[clap(skip)]
    pub pass_len: usize,
    #[clap(skip)]
    pub back_addr: Option<SocketAddr>,
    #[clap(skip)]
    pub dns_cache: HashMap<String, DnsEntry>,
    #[clap(skip)]
    pub udp_header_len: usize,
    #[clap(skip)]
    pub empty_addr: Option<SocketAddr>,
    #[clap(skip)]
    pub idle_duration: Duration,
}

impl Opts {
    pub fn setup(&mut self) {
        if self.mode == "server" {
            if self.cert.is_none() || self.key.is_none() {
                panic!("server mode require both cert and key file");
            }
            let back_addr: SocketAddr = self.remote_addr.parse().unwrap();
            self.back_addr = Some(back_addr);
        } else {
            if self.hostname.is_none() {
                panic!("proxy mode require hostname");
            }
            let mut hostname = self.hostname.as_ref().unwrap().clone();
            if !hostname.ends_with(".") {
                hostname.push('.');
            }
            let resolver = Resolver::from_system_conf().unwrap();
            let response = resolver.lookup_ip(hostname.as_str()).unwrap();
            while let Some(ip) = response.iter().next() {
                if ip.is_ipv4() {
                    self.back_addr.replace(SocketAddr::new(ip, 443));
                    break;
                } else if self.back_addr.is_none() {
                    self.back_addr.replace(SocketAddr::new(ip, 443));
                }
            }
            if self.back_addr.is_none() {
                panic!("resolve host {} failed", hostname);
            }

            log::info!("server address is {}", self.back_addr.as_ref().unwrap());
        }
        let empty_addr = if self.back_addr.as_ref().unwrap().is_ipv4() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        };
        self.empty_addr.replace(empty_addr);
        self.dns_cache_duration = Duration::new(self.dns_cache_time, 0);
        self.idle_duration = Duration::new(self.idle_timeout, 0);
        self.digest_pass();
    }

    fn digest_pass(&mut self) {
        let mut encoder = Sha224::new();
        self.sha_pass.clear();
        for pass in &self.password {
            encoder.reset();
            encoder.input(pass.as_bytes());
            let result = encoder.result_str();
            self.pass_len = result.len();
            log::info!("sha224({}) = {}, length = {}", pass, result, self.pass_len);
            self.sha_pass.push(result);
        }
    }

    pub fn check_pass(&self, pass: &str) -> Option<&String> {
        for i in 0..self.sha_pass.len() {
            if self.sha_pass[i].eq(pass) {
                return Some(&self.password[i]);
            }
        }
        None
    }

    pub fn get_pass(&self) -> &String {
        self.sha_pass.get(0).unwrap()
    }

    pub fn update_dns(&mut self, domain: String, address: IpAddr) {
        log::trace!("update dns cache, {} = {}", domain, address);
        let expired_time = Instant::now() + self.dns_cache_duration;
        self.dns_cache.insert(domain,
                              DnsEntry {
                                  address,
                                  expired_time,
                              });
    }

    pub fn query_dns(&mut self, domain: &String) -> Option<IpAddr> {
        if let Some(entry) = self.dns_cache.get(domain) {
            log::debug!("found {} = {} in dns cache", domain, entry.address);
            if entry.expired_time > Instant::now() {
                return Some(entry.address);
            } else {
                log::info!("domain {} expired, remove from cache", domain);
                let _ = self.dns_cache.remove(domain);
            }
        }
        None
    }
}

pub fn setup_logger(logfile: &Option<String>, level: u8) {
    let level = match level {
        0x00 => log::LevelFilter::Trace,
        0x01 => log::LevelFilter::Debug,
        0x02 => log::LevelFilter::Info,
        0x03 => log::LevelFilter::Warn,
        0x04 => log::LevelFilter::Error,
        _ => log::LevelFilter::Off,
    };
    let mut builder = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}:{}][{}]{}",
                chrono::Local::now().format("[%Y-%m-%d %H:%M:%S%.6f]"),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.level(),
                message
            ))
        })
        .level(level);
    if logfile.is_some() {
        builder = builder.chain(fern::log_file(logfile.as_ref().unwrap()).unwrap());
    } else {
        builder = builder.chain(std::io::stdout());
    }
    builder.apply().unwrap();
}

