//! 极简网络栈：ARP + IPv4 + ICMP。
//!
//! 目标不是"完整的 TCP/IP"，而是先把**链路打通**：
//! 能发 ARP 请求拿到网关 MAC，能 ping 通网关。这是空中升级的第一步。
//!
//! 地址用 QEMU/VirtualBox 用户态 NAT 的固定约定：10.0.2.15/24，网关 10.0.2.2。

use core::sync::atomic::{AtomicBool, Ordering};

pub const OUR_IP: [u8; 4] = [10, 0, 2, 15];
pub const GATEWAY: [u8; 4] = [10, 0, 2, 2];
const NETMASK: [u8; 4] = [255, 255, 255, 0];

static READY: AtomicBool = AtomicBool::new(false);

/// 最近一次收到的 ARP 应答（谁回的 MAC）
static mut ARP_REPLY_IP: [u8; 4] = [0; 4];
static mut ARP_REPLY_MAC: [u8; 6] = [0; 6];
static mut ARP_GOT: bool = false;

/// 最近一次收到的 ICMP 回显应答
static mut PING_GOT: bool = false;

pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// 初始化网卡。返回 Err 表示这台机器上没有可用的 e1000。
pub fn init() -> Result<(), &'static str> {
    crate::dev::e1000::init()?;
    READY.store(true, Ordering::Release);
    Ok(())
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// 发一个 ARP 请求，等应答。返回对端 MAC。
pub fn arp_resolve(target: [u8; 4]) -> Option<[u8; 6]> {
    if !ready() {
        return None;
    }
    let mac = crate::dev::e1000::mac();
    let mut f = [0u8; 42];
    // 以太网头
    f[0..6].copy_from_slice(&[0xFF; 6]); // 广播
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&0x0806u16.to_be_bytes()); // ARP
    // ARP 体
    f[14..16].copy_from_slice(&1u16.to_be_bytes()); // 以太网
    f[16..18].copy_from_slice(&0x0800u16.to_be_bytes()); // IPv4
    f[18] = 6;
    f[19] = 4;
    f[20..22].copy_from_slice(&1u16.to_be_bytes()); // request
    f[22..28].copy_from_slice(&mac);
    f[28..32].copy_from_slice(&OUR_IP);
    f[32..38].copy_from_slice(&[0; 6]);
    f[38..42].copy_from_slice(&target);

    unsafe {
        ARP_GOT = false;
    }
    let _ = crate::dev::e1000::send(&f);

    // 轮询 200 ms 左右
    for _ in 0..400 {
        crate::dev::e1000::poll(|pkt| {
            // ARP 应答：以太网类型 0x0806，op=2
            if pkt.len() >= 42 && pkt[12] == 0x08 && pkt[13] == 0x06 && pkt[20] == 0 && pkt[21] == 2 {
                if pkt[28..32] == target {
                    unsafe {
                        ARP_REPLY_IP.copy_from_slice(&pkt[28..32]);
                        ARP_REPLY_MAC.copy_from_slice(&pkt[22..28]);
                        ARP_GOT = true;
                    }
                }
            }
        });
        if unsafe { ARP_GOT } {
            return Some(unsafe { ARP_REPLY_MAC });
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    None
}

/// ping 一次（ICMP echo），等回包。
pub fn ping(target: [u8; 4]) -> bool {
    if !ready() {
        return false;
    }
    let dst_mac = match arp_resolve(target) {
        Some(m) => m,
        None => return false,
    };
    let mac = crate::dev::e1000::mac();
    let mut f = [0u8; 42 + 20 + 8];
    let n = f.len();

    // 以太网头
    f[0..6].copy_from_slice(&dst_mac);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&0x0800u16.to_be_bytes());

    // IPv4
    let ip_off = 14;
    f[ip_off] = 0x45; // v4, IHL=5
    f[ip_off + 1] = 0;
    let total = (n - 14) as u16;
    f[ip_off + 2..ip_off + 4].copy_from_slice(&total.to_be_bytes());
    f[ip_off + 4..ip_off + 6].copy_from_slice(&0x1234u16.to_be_bytes()); // id
    f[ip_off + 6..ip_off + 8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
    f[ip_off + 8] = 64; // TTL
    f[ip_off + 9] = 1; // ICMP
    f[ip_off + 12..ip_off + 16].copy_from_slice(&OUR_IP);
    f[ip_off + 16..ip_off + 20].copy_from_slice(&target);
    let ck = checksum(&f[ip_off..ip_off + 20]);
    f[ip_off + 10..ip_off + 12].copy_from_slice(&ck.to_be_bytes());

    // ICMP echo request
    let ic = ip_off + 20;
    f[ic] = 8; // echo request
    f[ic + 1] = 0;
    f[ic + 4..ic + 6].copy_from_slice(&0x5A5Au16.to_be_bytes());
    let ck = checksum(&f[ic..n]);
    f[ic + 2..ic + 4].copy_from_slice(&ck.to_be_bytes());

    unsafe {
        PING_GOT = false;
    }
    let _ = crate::dev::e1000::send(&f);

    for _ in 0..600 {
        crate::dev::e1000::poll(|pkt| {
            if pkt.len() >= ic + 8 && pkt[12] == 0x08 && pkt[13] == 0x00 && pkt[23] == 1 {
                // ICMP echo reply
                if pkt[ic] == 0 {
                    unsafe {
                        PING_GOT = true;
                    }
                }
            }
        });
        if unsafe { PING_GOT } {
            return true;
        }
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
    false
}

/// 诊断用：返回 (MAC, 链路是否 up)
pub fn link() -> ([u8; 6], bool) {
    let (up, _) = crate::dev::e1000::status();
    (crate::dev::e1000::mac(), up)
}
