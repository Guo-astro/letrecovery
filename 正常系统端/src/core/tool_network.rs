//! 工具箱网络信息与重置实现。
//!
//! 提供网络信息获取和网络重置等功能

use crate::tr;
use crate::utils::cmd::create_command;

/// Physical network source that can carry PE networking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeNetworkSource {
    Wired,
    Wireless,
    /// USB tethering from a phone (Android RNDIS/NCM, Apple Mobile Device Ethernet).
    PhoneTethering,
}

/// Name/description fragments of adapters that are not a physical network source: proxy, VPN and
/// tunnel adapters (Clash/Mihomo/sing-box TUN, Wintun, WireGuard, TAP, ZeroTier, Tailscale ...),
/// host-side virtual switches of VMware, Hyper-V and VirtualBox, and Bluetooth PAN, for which
/// WinPE has no stack. Guest NICs inside a virtual machine ("Microsoft Hyper-V Network Adapter",
/// "vmxnet3 Ethernet Adapter", "Intel(R) 82574L", "Red Hat VirtIO") are real sources and stay.
const NON_PHYSICAL_ADAPTER_HINTS: &[&str] = &[
    "virtual",
    "vethernet",
    "default switch",
    "host-only",
    "loopback",
    "clash",
    "mihomo",
    "sing-box",
    "singbox",
    "tun",
    "tap-",
    "wireguard",
    "tailscale",
    "zerotier",
    "openvpn",
    "vpn",
    "hamachi",
    "npcap",
    "miniport",
    "bluetooth",
    "teredo",
    "isatap",
];

/// Description fragments of USB tethering adapters exposed by phones.
const PHONE_TETHERING_HINTS: &[&str] = &[
    "remote ndis",
    "rndis",
    "usbncm",
    "apple mobile device ethernet",
];

const WIRELESS_TYPE_HINTS: &[&str] = &["wireless", "wi-fi", "wlan", "802.11"];

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// Classify one adapter as a PE network source, or `None` when it cannot carry PE networking.
///
/// The displayed type and status come from `tr!`, so they are compared with both the Chinese
/// source text and the active UI language; the former exact Chinese comparison rejected every
/// adapter whenever LetRecovery ran in English, Japanese, Korean, French or German. The former
/// "hyper-v" hint also rejected the only NIC of a Hyper-V guest, not just the host vSwitch.
pub fn pe_network_source(
    adapter: &crate::core::hardware_info::NetworkAdapterInfo,
) -> Option<PeNetworkSource> {
    let haystack = format!("{} {}", adapter.name, adapter.description).to_ascii_lowercase();
    let adapter_type = adapter.adapter_type.as_str();
    let type_lower = adapter_type.to_ascii_lowercase();
    let wired = adapter_type == "以太网"
        || adapter_type == tr!("以太网")
        || type_lower.contains("ethernet");
    let wireless = adapter_type == "无线网络"
        || adapter_type == tr!("无线网络")
        || contains_any(&type_lower, WIRELESS_TYPE_HINTS);
    let connected = adapter.status == "已连接" || adapter.status == tr!("已连接");
    let non_physical = contains_any(&haystack, NON_PHYSICAL_ADAPTER_HINTS);
    if non_physical || !connected || adapter.ip_addresses.is_empty() {
        return None;
    }
    if wired && contains_any(&haystack, PHONE_TETHERING_HINTS) {
        Some(PeNetworkSource::PhoneTethering)
    } else if wireless {
        Some(PeNetworkSource::Wireless)
    } else if wired {
        Some(PeNetworkSource::Wired)
    } else {
        None
    }
}

/// Select adapters that can carry PE networking: the connected physical source of the current
/// network (Ethernet, Wi-Fi, USB phone tethering, or the NIC of a VMware / Hyper-V / KVM guest).
/// The selector is pure so the same policy can be used by the normal endpoint and its tests
/// without probing or mutating the network stack.
pub fn select_pe_network_adapters(
    adapters: &[crate::core::hardware_info::NetworkAdapterInfo],
) -> Vec<crate::core::hardware_info::NetworkAdapterInfo> {
    adapters
        .iter()
        .filter(|adapter| pe_network_source(adapter).is_some())
        .cloned()
        .collect()
}

#[cfg(test)]
mod pe_network_tests {
    use super::{pe_network_source, select_pe_network_adapters, PeNetworkSource};
    use crate::core::hardware_info::NetworkAdapterInfo;

    fn adapter(name: &str, description: &str, kind: &str) -> NetworkAdapterInfo {
        NetworkAdapterInfo {
            name: name.into(),
            description: description.into(),
            adapter_type: kind.into(),
            status: "已连接".into(),
            ip_addresses: vec!["192.0.2.10".into()],
            ..Default::default()
        }
    }

    #[test]
    fn excludes_virtual_tunnels_and_keeps_physical_ethernet_and_wifi() {
        let candidates = vec![
            adapter("Ethernet", "Intel Ethernet Controller", "以太网"),
            adapter("Wi-Fi", "Qualcomm Wireless Adapter", "无线网络"),
            adapter(
                "vEthernet (Default Switch)",
                "Hyper-V Virtual Ethernet",
                "以太网",
            ),
            adapter("Clash", "Clash TUN Adapter", "隧道"),
        ];
        let selected = select_pe_network_adapters(&candidates);
        assert_eq!(selected.len(), 2);
        assert!(selected
            .iter()
            .all(|item| item.name == "Ethernet" || item.name == "Wi-Fi"));
    }

    #[test]
    fn keeps_real_sources_including_virtual_machine_nics_and_phone_tethering() {
        let candidates = vec![
            adapter("以太网", "Microsoft Hyper-V Network Adapter", "以太网"),
            adapter("Ethernet0", "vmxnet3 Ethernet Adapter", "以太网"),
            adapter(
                "Ethernet1",
                "Intel(R) 82574L Gigabit Network Connection",
                "以太网",
            ),
            adapter("Ethernet 2", "Red Hat VirtIO Ethernet Adapter", "以太网"),
            adapter("WLAN", "Intel(R) Wi-Fi 6 AX201 160MHz", "无线网络"),
            adapter(
                "以太网 3",
                "Remote NDIS based Internet Sharing Device",
                "以太网",
            ),
            adapter("Ethernet 4", "Apple Mobile Device Ethernet", "以太网"),
        ];
        let expected = [
            PeNetworkSource::Wired,
            PeNetworkSource::Wired,
            PeNetworkSource::Wired,
            PeNetworkSource::Wired,
            PeNetworkSource::Wireless,
            PeNetworkSource::PhoneTethering,
            PeNetworkSource::PhoneTethering,
        ];
        for (adapter, source) in candidates.iter().zip(expected) {
            assert_eq!(pe_network_source(adapter), Some(source));
        }
        assert_eq!(
            select_pe_network_adapters(&candidates).len(),
            candidates.len()
        );
    }

    #[test]
    fn rejects_proxy_vpn_tunnel_host_switch_and_bluetooth_adapters() {
        let candidates = vec![
            adapter(
                "vEthernet (Default Switch)",
                "Hyper-V Virtual Ethernet Adapter",
                "以太网",
            ),
            adapter(
                "VMware Network Adapter VMnet8",
                "VMware Virtual Ethernet Adapter for VMnet8",
                "以太网",
            ),
            adapter(
                "VirtualBox Host-Only Network",
                "VirtualBox Host-Only Ethernet Adapter",
                "以太网",
            ),
            adapter("Meta", "Meta Tunnel", "以太网"),
            adapter("Mihomo", "Mihomo", "以太网"),
            adapter("wg0", "WireGuard Tunnel", "以太网"),
            adapter("ZeroTier One", "ZeroTier Virtual Port", "以太网"),
            adapter("以太网 5", "TAP-Windows Adapter V9", "以太网"),
            adapter(
                "蓝牙网络连接",
                "Bluetooth Device (Personal Area Network)",
                "以太网",
            ),
        ];
        for adapter in &candidates {
            assert_eq!(pe_network_source(adapter), None);
        }
        assert!(select_pe_network_adapters(&candidates).is_empty());
    }

    #[test]
    fn requires_a_connected_adapter_with_an_address() {
        let mut disconnected = adapter("Ethernet", "Intel Ethernet Controller", "以太网");
        disconnected.status = "已断开".into();
        let mut unaddressed = adapter("WLAN", "Intel(R) Wi-Fi 6 AX201", "无线网络");
        unaddressed.ip_addresses.clear();
        assert_eq!(pe_network_source(&disconnected), None);
        assert_eq!(pe_network_source(&unaddressed), None);
    }
}

/// 使用 Windows API 获取详细的网络信息
pub fn get_detailed_network_info() -> Vec<crate::core::hardware_info::NetworkAdapterInfo> {
    let mut adapters = Vec::new();

    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKET_ADDRESS {
            lpSockaddr: *mut std::ffi::c_void,
            iSockaddrLength: i32,
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct IP_ADAPTER_UNICAST_ADDRESS {
            Length: u32,
            Flags: u32,
            Next: *mut IP_ADAPTER_UNICAST_ADDRESS,
            Address: SOCKET_ADDRESS,
            PrefixOrigin: i32,
            SuffixOrigin: i32,
            DadState: i32,
            ValidLifetime: u32,
            PreferredLifetime: u32,
            LeaseLifetime: u32,
            OnLinkPrefixLength: u8,
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct IP_ADAPTER_ADDRESSES {
            Length: u32,
            IfIndex: u32,
            Next: *mut IP_ADAPTER_ADDRESSES,
            AdapterName: *const i8,
            FirstUnicastAddress: *mut IP_ADAPTER_UNICAST_ADDRESS,
            FirstAnycastAddress: *mut std::ffi::c_void,
            FirstMulticastAddress: *mut std::ffi::c_void,
            FirstDnsServerAddress: *mut std::ffi::c_void,
            DnsSuffix: *const u16,
            Description: *const u16,
            FriendlyName: *const u16,
            PhysicalAddress: [u8; 8],
            PhysicalAddressLength: u32,
            Flags: u32,
            Mtu: u32,
            IfType: u32,
            OperStatus: i32,
            Ipv6IfIndex: u32,
            ZoneIndices: [u32; 16],
            FirstPrefix: *mut std::ffi::c_void,
            TransmitLinkSpeed: u64,
            ReceiveLinkSpeed: u64,
        }

        #[link(name = "iphlpapi")]
        extern "system" {
            fn GetAdaptersAddresses(
                Family: u32,
                Flags: u32,
                Reserved: *mut std::ffi::c_void,
                AdapterAddresses: *mut IP_ADAPTER_ADDRESSES,
                SizePointer: *mut u32,
            ) -> u32;
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKADDR_IN {
            sin_family: u16,
            sin_port: u16,
            sin_addr: [u8; 4],
            sin_zero: [u8; 8],
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKADDR_IN6 {
            sin6_family: u16,
            sin6_port: u16,
            sin6_flowinfo: u32,
            sin6_addr: [u8; 16],
            sin6_scope_id: u32,
        }

        const AF_UNSPEC: u32 = 0;
        const GAA_FLAG_INCLUDE_PREFIX: u32 = 0x0010;

        unsafe {
            let mut buf_len: u32 = 0;
            let result = GetAdaptersAddresses(
                AF_UNSPEC,
                GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut buf_len,
            );

            // ERROR_BUFFER_OVERFLOW = 111
            if result != 111 && result != 0 {
                return adapters;
            }

            if buf_len == 0 {
                return adapters;
            }

            let mut buffer: Vec<u8> = vec![0u8; buf_len as usize];
            let adapter_addresses = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES;

            let result = GetAdaptersAddresses(
                AF_UNSPEC,
                GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null_mut(),
                adapter_addresses,
                &mut buf_len,
            );

            if result != 0 {
                return adapters;
            }

            let mut current = adapter_addresses;
            while !current.is_null() {
                let adapter = &*current;

                // 获取友好名称
                let friendly_name = if !adapter.FriendlyName.is_null() {
                    let mut len = 0;
                    let mut ptr = adapter.FriendlyName;
                    while *ptr != 0 {
                        len += 1;
                        ptr = ptr.add(1);
                    }
                    let slice = std::slice::from_raw_parts(adapter.FriendlyName, len);
                    OsString::from_wide(slice).to_string_lossy().to_string()
                } else {
                    String::new()
                };

                // 获取描述
                let description = if !adapter.Description.is_null() {
                    let mut len = 0;
                    let mut ptr = adapter.Description;
                    while *ptr != 0 {
                        len += 1;
                        ptr = ptr.add(1);
                    }
                    let slice = std::slice::from_raw_parts(adapter.Description, len);
                    OsString::from_wide(slice).to_string_lossy().to_string()
                } else {
                    String::new()
                };

                // 获取MAC地址
                let mac = if adapter.PhysicalAddressLength > 0 {
                    adapter.PhysicalAddress[..adapter.PhysicalAddressLength as usize]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    String::new()
                };

                // 获取IP地址
                let mut ip_addresses = Vec::new();
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let unicast_addr = &*unicast;
                    if !unicast_addr.Address.lpSockaddr.is_null() {
                        let family = *(unicast_addr.Address.lpSockaddr as *const u16);

                        // AF_INET = 2 (IPv4)
                        if family == 2 {
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN;
                            let addr = (*sockaddr).sin_addr;
                            let ip = format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
                            if ip != "0.0.0.0" {
                                ip_addresses.push(ip);
                            }
                        }
                        // AF_INET6 = 23 (IPv6)
                        else if family == 23 {
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN6;
                            let addr = (*sockaddr).sin6_addr;
                            let ipv6 = format!(
                                "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                                addr[0], addr[1], addr[2], addr[3], addr[4], addr[5], addr[6], addr[7],
                                addr[8], addr[9], addr[10], addr[11], addr[12], addr[13], addr[14], addr[15]
                            );
                            // 过滤全零地址
                            if !ipv6.starts_with("0000:0000:0000:0000") {
                                ip_addresses.push(ipv6);
                            }
                        }
                    }
                    unicast = unicast_addr.Next;
                }

                // 获取适配器类型
                let adapter_type = match adapter.IfType {
                    6 => tr!("以太网"),
                    71 => tr!("无线网络"),
                    24 => tr!("回环"),
                    131 => tr!("隧道"),
                    _ => tr!("类型 {}", adapter.IfType),
                };

                // 获取状态
                let status = match adapter.OperStatus {
                    1 => tr!("已连接"),
                    2 => tr!("已断开"),
                    3 => tr!("测试中"),
                    4 => tr!("未知"),
                    5 => tr!("休眠"),
                    6 => tr!("未启用"),
                    7 => tr!("下层关闭"),
                    _ => tr!("未知"),
                };

                // 过滤掉回环适配器和空描述的适配器
                if adapter.IfType != 24 && !description.is_empty() {
                    let speed = query_interface_link_speed(
                        adapter.IfIndex,
                        adapter.TransmitLinkSpeed,
                        adapter.ReceiveLinkSpeed,
                    );
                    adapters.push(crate::core::hardware_info::NetworkAdapterInfo {
                        name: friendly_name,
                        description,
                        mac_address: mac,
                        ip_addresses,
                        adapter_type,
                        status,
                        speed,
                    });
                }

                current = adapter.Next;
            }
        }
    }

    adapters
}

fn preferred_link_speed(
    address_tx: u64,
    address_rx: u64,
    interface_tx: u64,
    interface_rx: u64,
    legacy_speed: u32,
) -> u64 {
    address_tx
        .max(address_rx)
        .max(interface_tx.max(interface_rx))
        .max(u64::from(legacy_speed))
}

#[cfg(windows)]
unsafe fn query_interface_link_speed(if_index: u32, address_tx: u64, address_rx: u64) -> u64 {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetIfEntry, GetIfEntry2, MIB_IFROW, MIB_IF_ROW2,
    };

    // Keep GetAdaptersAddresses as the zero-extra-call path on modern Windows. Windows 7 network
    // drivers can leave those two fields at zero even though the interface table reports the
    // negotiated link rate.
    let address_speed = address_tx.max(address_rx);
    if address_speed != 0 {
        return address_speed;
    }

    let mut row2 = MIB_IF_ROW2 {
        InterfaceIndex: if_index,
        ..Default::default()
    };
    let (interface_tx, interface_rx) = if GetIfEntry2(&mut row2).0 == 0 {
        (row2.TransmitLinkSpeed, row2.ReceiveLinkSpeed)
    } else {
        (0, 0)
    };
    if interface_tx != 0 || interface_rx != 0 {
        return interface_tx.max(interface_rx);
    }

    // The legacy table is available before Windows 7 and remains a final compatibility fallback.
    // Its 32-bit bits-per-second field can saturate above 4 Gbps, so it is never preferred over the
    // 64-bit modern values.
    let mut legacy = MIB_IFROW {
        dwIndex: if_index,
        ..Default::default()
    };
    let legacy_speed = if GetIfEntry(&mut legacy) == 0 {
        legacy.dwSpeed
    } else {
        0
    };
    preferred_link_speed(
        address_tx,
        address_rx,
        interface_tx,
        interface_rx,
        legacy_speed,
    )
}

/// 执行网络重置
pub fn reset_network() -> (usize, usize) {
    let commands = [
        ("netsh", &["winsock", "reset"][..]),
        ("netsh", &["int", "ip", "reset"][..]),
        ("ipconfig", &["/flushdns"][..]),
        ("netsh", &["advfirewall", "reset"][..]),
    ];

    let mut success_count = 0;
    let mut fail_count = 0;

    for (cmd, args) in &commands {
        match create_command(cmd).args(*args).output() {
            Ok(output) => {
                if output.status.success() {
                    success_count += 1;
                } else {
                    fail_count += 1;
                }
            }
            Err(_) => {
                fail_count += 1;
            }
        }
    }

    (success_count, fail_count)
}

#[cfg(test)]
mod tests {
    use super::preferred_link_speed;

    #[test]
    fn adapter_address_speed_remains_the_fast_preferred_value() {
        assert_eq!(
            preferred_link_speed(2_500_000_000, 1_000_000_000, 0, 0, 100_000_000),
            2_500_000_000
        );
    }

    #[test]
    fn windows7_interface_and_legacy_fallbacks_replace_zero_mbps() {
        assert_eq!(
            preferred_link_speed(0, 0, 1_000_000_000, 1_000_000_000, 100_000_000),
            1_000_000_000
        );
        assert_eq!(preferred_link_speed(0, 0, 0, 0, 100_000_000), 100_000_000);
    }
}
