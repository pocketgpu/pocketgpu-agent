//! Who listens on a model server's port. On a shared Linux server another person can listen on
//! Ollama's or vLLM's port while the server is down, then read the phone's chats and answer in
//! its place. So before anything goes to a port on this machine, the program listening there
//! must run as the agent's user, root or a system user (services such as `ollama` run as one);
//! a port held by another person's account is refused. macOS and Windows aren't looked at:
//! there the owner of another user's socket can't be read without admin rights.

/// Why nothing may go to `port` on this machine, if it may not.
pub fn refusal(port: u16) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let uid = linux::stranger(port)?;
        let who = linux::user_name(uid).unwrap_or_else(|| format!("user {uid}"));
        Some(format!(
            "port {port} is held by a program of {who}, not of this agent's user, so PocketGPU \
             won't send it anything"
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = port;
        None
    }
}

/// The owners of sockets listening on `port` where 127.0.0.1 reaches them, from the text of
/// `/proc/net/tcp` or `/proc/net/tcp6`.
#[cfg(any(target_os = "linux", test))]
fn listeners(table: &str, port: u16) -> Vec<u32> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    // Each address word is printed as the machine's own number of the bytes on the wire.
    let word = |hex: &str| u32::from_str_radix(hex, 16).ok().map(u32::to_ne_bytes);
    let addr = |hex: &str| -> Option<IpAddr> {
        match hex.len() {
            8 => Some(Ipv4Addr::from(word(hex)?).into()),
            32 => {
                let mut bytes = [0u8; 16];
                for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                    chunk.copy_from_slice(&word(&hex[i * 8..i * 8 + 8])?);
                }
                Some(Ipv6Addr::from(bytes).into())
            }
            _ => None,
        }
    };
    let reaches = |ip: IpAddr| match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
    };
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            // sl, local address, remote address, state (0A: listening), queues, timer,
            // retransmits, uid.
            let (local, state, uid) = (f.get(1)?, f.get(3)?, f.get(7)?);
            let (ip, p) = local.split_once(':')?;
            let listening = *state == "0A" && u16::from_str_radix(p, 16).ok()? == port;
            (listening && reaches(addr(ip)?)).then(|| uid.parse().ok())?
        })
        .collect()
}

/// Whether a socket of `uid` may serve the agent: its own user, root, a system user (below
/// the first person's id), or the `ollama` user however it was made.
#[cfg(any(target_os = "linux", test))]
fn trusted(uid: u32, me: u32, first_person: u32, ollama: Option<u32>) -> bool {
    uid == me || uid == 0 || uid < first_person || Some(uid) == ollama
}

#[cfg(target_os = "linux")]
mod linux {
    /// The owner of a socket on `port` that the agent shouldn't talk to, if there is one.
    pub fn stranger(port: u16) -> Option<u32> {
        let mut owners = Vec::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(text) = std::fs::read_to_string(table) {
                owners.extend(super::listeners(&text, port));
            }
        }
        if owners.is_empty() {
            return None;
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        let (first, ollama) = (first_person(), uid_of("ollama"));
        owners
            .into_iter()
            .find(|&uid| !super::trusted(uid, me, first, ollama))
    }

    /// `UID_MIN` in /etc/login.defs: the first id given to a person's account.
    fn first_person() -> u32 {
        std::fs::read_to_string("/etc/login.defs")
            .ok()
            .and_then(|text| {
                text.lines().find_map(|l| {
                    let mut f = l.split_whitespace();
                    (f.next() == Some("UID_MIN")).then(|| f.next()?.parse::<u32>().ok())?
                })
            })
            .unwrap_or(1000)
    }

    /// Names and ids from /etc/passwd.
    fn passwd() -> Vec<(String, u32)> {
        std::fs::read_to_string("/etc/passwd")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split(':');
                let name = f.next()?.to_string();
                Some((name, f.nth(1)?.parse::<u32>().ok()?))
            })
            .collect()
    }

    fn uid_of(name: &str) -> Option<u32> {
        passwd()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, uid)| uid)
    }

    pub fn user_name(uid: u32) -> Option<String> {
        passwd()
            .into_iter()
            .find(|(_, u)| *u == uid)
            .map(|(n, _)| n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/proc/net/tcp` line as this machine would print it.
    fn v4(ip: [u8; 4], port: u16, state: &str, uid: u32) -> String {
        format!(
            "   0: {:08X}:{port:04X} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 {uid:>5} 0 1 1 0 100 0 0 10 0",
            u32::from_ne_bytes(ip)
        )
    }

    fn v6(ip: std::net::Ipv6Addr, port: u16, uid: u32) -> String {
        let hex: String = ip
            .octets()
            .chunks(4)
            .map(|c| format!("{:08X}", u32::from_ne_bytes(c.try_into().unwrap())))
            .collect();
        format!(
            "   0: {hex}:{port:04X} 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 {uid:>5} 0 1 1 0"
        )
    }

    const HEAD: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    #[test]
    fn finds_who_listens_where_127_0_0_1_reaches() {
        let table = [
            HEAD.to_string(),
            v4([127, 0, 0, 1], 11434, "0A", 1003),
            v4([0, 0, 0, 0], 8000, "0A", 0),
            // Connected, not listening; another port; another address.
            v4([127, 0, 0, 1], 11434, "01", 1004),
            v4([127, 0, 0, 1], 1234, "0A", 1005),
            v4([10, 0, 0, 5], 11434, "0A", 1006),
        ]
        .join("\n");
        assert_eq!(listeners(&table, 11434), vec![1003]);
        assert_eq!(listeners(&table, 8000), vec![0]);
        assert_eq!(listeners(&table, 9999), Vec::<u32>::new());

        let table6 = [
            HEAD.to_string(),
            v6(std::net::Ipv6Addr::UNSPECIFIED, 8080, 1001),
            v6(std::net::Ipv6Addr::LOCALHOST, 8080, 1002),
            v6("::ffff:127.0.0.1".parse().unwrap(), 8080, 1003),
            v6("fd00::1".parse().unwrap(), 8080, 1004),
        ]
        .join("\n");
        assert_eq!(listeners(&table6, 8080), vec![1001, 1002, 1003]);
    }

    #[test]
    fn only_people_other_than_the_agents_user_are_strangers() {
        let ollama = Some(1500);
        assert!(trusted(1000, 1000, 1000, ollama), "the agent's own user");
        assert!(trusted(0, 1000, 1000, ollama), "root");
        assert!(trusted(998, 1000, 1000, ollama), "a system user");
        assert!(trusted(1500, 1000, 1000, ollama), "the ollama user");
        assert!(!trusted(1001, 1000, 1000, ollama), "another person");
        assert!(!trusted(1001, 1000, 1000, None));
    }
}
