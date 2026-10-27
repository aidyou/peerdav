//! 稳定机器码：把本机「除非更换核心硬件、重装系统否则不变」的系统标识哈希成设备身份。
//!
//! 为什么不用 IP：IP 由网络分配，换个 DHCP 租约或换张网卡就会变，用它当授权标志等于
//! 把权限绑在路由器上。为什么不用 CPU 序列号：x86 自 Pentium III 起不再提供可读的
//! CPU 序列号（CPUID 里没有稳定唯一值），ARM SoC 的序列号也不可移植，因此这里取
//! 各平台真正的硬件/安装级 UUID：
//!
//! - Linux：`/etc/machine-id`（无 systemd 时退回 `/var/lib/dbus/machine-id`），
//!   两者都缺失时退回主网卡 MAC。
//! - macOS：`IOPlatformUUID`（ioreg 读取，主板级硬件 UUID）。
//! - Windows：`HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`。
//!
//! 机器码不是密钥：它只用于「这台设备是哪台设备」的比对，拿到它也无法据此访问共享，
//! 访问仍然需要授权码。哈希只用于避免把原始硬件标识（MAC/UUID）直接暴露给对端。

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// 机器码长度（hex 字符，即 128 位摘要）。
pub const MACHINE_CODE_LEN: usize = 32;
const HASH_DOMAIN: &[u8] = b"peerdav-machine-v1:";
const MAX_RAW_CHARS: usize = 128;

static CACHED_CODE: OnceLock<Option<String>> = OnceLock::new();

/// 本机机器码；取不到任何稳定标识时返回 `None`，调用方应回退到持久化安装 id。
/// 结果在进程内缓存：同一进程里机器码必须恒定，否则同一设备会在服务端留下多份记录。
pub fn machine_code() -> Option<String> {
    CACHED_CODE
        .get_or_init(|| platform_identifier().map(|raw| hash_identifier(&raw)))
        .clone()
}

/// 机器码的短展示形式：前 8 位 + 后 4 位，便于在列表里比对，完整值仍可在详情查看。
pub fn short_code(code: &str) -> String {
    let trimmed = code.trim();
    if trimmed.len() <= 16 {
        return trimmed.to_owned();
    }
    format!("{}…{}", &trimmed[..8], &trimmed[trimmed.len() - 4..])
}

/// 把原始标识（系统 UUID 或 MAC）哈希成机器码。输入先去掉首尾空白并统一小写，
/// 因此大小写或换行差异不会让同一台设备算成两台。
pub fn hash_identifier(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(raw.trim().to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..MACHINE_CODE_LEN / 2])
}

/// 机器码是否可用于身份比对：固定长度的小写 hex。
pub fn valid_machine_code(code: &str) -> bool {
    let trimmed = code.trim();
    trimmed.len() == MACHINE_CODE_LEN
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 归一化对端上报的机器码；格式不对时视为「未上报」，退回 IP 身份。
pub fn normalize_reported(code: Option<&str>) -> Option<String> {
    let trimmed = code?.trim().to_ascii_lowercase();
    valid_machine_code(&trimmed).then_some(trimmed)
}

/// 读取文件并去掉首尾空白；内容为空或过长时视为无效。
#[cfg(target_os = "linux")]
fn read_identifier_file(path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_RAW_CHARS {
        return None;
    }
    // 系统标识是 ASCII UUID/hex；出现控制字符或空格说明读到了不该读的内容。
    if trimmed
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// 判断 `/sys/class/net/<iface>/address` 读到的值是否是一个可用的单播 MAC。
/// 全零（未初始化）、组播位（虚拟接口）和全 F 都跳过。
#[cfg(target_os = "linux")]
fn valid_unicast_mac(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let octets: Vec<&str> = normalized.split(':').collect();
    if octets.len() != 6 {
        return None;
    }
    if octets
        .iter()
        .any(|octet| octet.len() != 2 || !octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return None;
    }
    if octets.iter().all(|octet| *octet == "00") || octets.iter().all(|octet| *octet == "ff") {
        return None;
    }
    let first = u8::from_str_radix(octets[0], 16).ok()?;
    if first & 0b1 != 0 {
        return None;
    }
    Some(normalized)
}

#[cfg(target_os = "linux")]
fn platform_identifier() -> Option<String> {
    // machine-id 是安装级标识：换网卡、换 IP 都不变，只有在更换系统盘或重装系统时才变。
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Some(value) = read_identifier_file(path) {
            return Some(value);
        }
    }
    primary_mac()
}

/// 主网卡 MAC：按接口名排序后取第一个可用单播 MAC，避免 `read_dir` 顺序不稳定导致
/// 同一台机器算出的机器码来回变化。
#[cfg(target_os = "linux")]
fn primary_mac() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != "lo")
        .collect();
    names.sort();
    for name in names {
        let path = format!("/sys/class/net/{name}/address");
        if let Some(mac) = read_identifier_file(&path).and_then(|value| valid_unicast_mac(&value)) {
            return Some(mac);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn platform_identifier() -> Option<String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_platform_uuid(&text)
}

/// 从 `ioreg` 输出里取出 IOPlatformUUID 的值。
#[cfg(target_os = "macos")]
fn parse_platform_uuid(output: &str) -> Option<String> {
    const KEY: &str = "\"IOPlatformUUID\"";
    let line = output.lines().find(|line| line.contains(KEY))?;
    let (_, value) = line.split_once('=')?;
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() || value.chars().count() > MAX_RAW_CHARS {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(target_os = "windows")]
fn platform_identifier() -> Option<String> {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW：这个查询在每台机器上只跑一次，不该闪一个控制台窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_reg_value(&text)
}

/// 从 `reg query` 输出里取出 MachineGuid：形如
/// `    MachineGuid    REG_SZ    0f0c...`，取最后一列。
#[cfg(target_os = "windows")]
fn parse_reg_value(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("MachineGuid") {
            continue;
        }
        let kind = fields.next()?;
        if !kind.starts_with("REG_") {
            return None;
        }
        let value = fields.next()?.trim();
        if value.is_empty() || value.chars().count() > MAX_RAW_CHARS {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_identifier() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_is_stable_and_format_stable() {
        let first = hash_identifier("0f0c4ed1-9a2e-4b3c-8d1f-1234567890ab");
        let second = hash_identifier("0f0c4ed1-9a2e-4b3c-8d1f-1234567890ab");
        assert_eq!(first, second);
        assert_eq!(first.len(), MACHINE_CODE_LEN);
        assert!(valid_machine_code(&first));
        // 大小写与空白差异不能把同一台设备算成两台。
        assert_eq!(
            first,
            hash_identifier("  0F0C4ED1-9A2E-4B3C-8D1F-1234567890AB\n")
        );
        // 不同来源必须得到不同机器码。
        assert_ne!(
            first,
            hash_identifier("0f0c4ed1-9a2e-4b3c-8d1f-1234567890ac")
        );
    }

    #[test]
    fn reported_machine_codes_are_normalized_or_rejected() {
        let valid = hash_identifier("abc");
        assert_eq!(
            normalize_reported(Some(&valid.to_ascii_uppercase())),
            Some(valid.clone())
        );
        assert_eq!(
            normalize_reported(Some(&format!("  {valid}  "))),
            Some(valid)
        );
        // 长度不符、非 hex、空值都视为「未上报」，服务端退回 IP 身份。
        for bad in [
            "",
            "abc",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "0123456789abcde",
        ] {
            assert_eq!(normalize_reported(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(normalize_reported(None), None);
    }

    #[test]
    fn short_codes_stay_readable() {
        let code = hash_identifier("some-hardware-id");
        let short = short_code(&code);
        assert!(short.starts_with(&code[..8]));
        assert!(short.ends_with(&code[code.len() - 4..]));
        // 短码比完整机器码短，但仍能区分相邻值（"…" 是多字节字符，按字符数算）。
        assert_eq!(short.chars().count(), 13);
        // 已经够短的输入不截断。
        assert_eq!(short_code("abcd"), "abcd");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mac_validation_rejects_unusable_interfaces() {
        assert_eq!(
            valid_unicast_mac("A4:5E:60:12:34:56"),
            Some("a4:5e:60:12:34:56".to_owned())
        );
        // 全零（未初始化）、组播位（虚拟接口）、全 F、格式错误都不可用。
        for bad in [
            "00:00:00:00:00:00",
            "01:00:5e:00:00:01",
            "ff:ff:ff:ff:ff:ff",
            "a4:5e:60:12:34",
            "a4:5e:60:12:34:5g",
            "",
        ] {
            assert_eq!(valid_unicast_mac(bad), None, "{bad:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn control_characters_in_identifier_files_are_rejected() {
        let path = std::env::temp_dir().join(format!("peerdav-machine-{}", std::process::id()));
        std::fs::write(&path, "abc def\n").expect("write probe file");
        assert_eq!(read_identifier_file(&path.to_string_lossy()), None);
        std::fs::write(&path, "0f0c4ed1-9a2e-4b3c-8d1f-1234567890ab\n").expect("write probe file");
        assert_eq!(
            read_identifier_file(&path.to_string_lossy()).as_deref(),
            Some("0f0c4ed1-9a2e-4b3c-8d1f-1234567890ab")
        );
        std::fs::write(&path, "").expect("write probe file");
        assert_eq!(read_identifier_file(&path.to_string_lossy()), None);
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            read_identifier_file("/nonexistent/peerdav-machine-id"),
            None
        );
    }

    #[test]
    fn machine_code_is_stable_within_the_process() {
        let first = machine_code();
        let second = machine_code();
        assert_eq!(first, second);
        if let Some(code) = first {
            assert!(valid_machine_code(&code), "{code}");
        }
    }
}
