//! Linux socket classification for `telemetry_default_opens_no_socket` (design §3.5: no
//! listener and no connection with `sink = "none"` and `harness_native = false`).
//!
//! A `socket:[INODE]` descriptor of a process says nothing of what the socket is; its inode is
//! looked up in the process's own tables under `/proc/<pid>/net`. On Linux the standard
//! library's `Command::spawn` takes the fork path whenever the command sets a process group,
//! and that path holds an unnamed CLOEXEC `AF_UNIX` `SOCK_SEQPACKET` pair to carry the child's
//! exec error back: that plumbing is no listener and no connection, and is ignored. Every
//! other socket that could listen or connect is a violation.

use std::path::Path;

/// The tables under `/proc/<pid>/net` an inode is looked up in.
pub const TABLES: [&str; 13] = [
    "unix", "tcp", "tcp6", "udp", "udp6", "udplite", "udplite6", "raw", "raw6", "icmp", "icmp6",
    "packet", "netlink",
];

/// `SOCK_SEQPACKET` in the `Type` column of `/proc/net/unix`.
const SEQPACKET: &str = "0005";
/// `SS_CONNECTED` in the `St` column of `/proc/net/unix`.
const CONNECTED: &str = "03";

/// A socket that listens or connects, as its table describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub inode: u64,
    pub table: String,
    /// The `Type` column of `unix` and `packet`, the `Eth` (protocol) column of `netlink`, the
    /// table's own name otherwise.
    pub kind: String,
    /// The `St` column of `unix` and the inet tables, `-` where the table has none.
    pub state: String,
    /// The `Path` of a `unix` socket, the local and remote address of an inet socket, empty
    /// otherwise.
    pub path: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "socket inode {} in /proc/<pid>/net/{}: type {}, state {}, path {:?}",
            self.inode, self.table, self.kind, self.state, self.path
        )
    }
}

/// What the tables say of a list of socket inodes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Judged {
    pub violations: Vec<Violation>,
    /// Inodes present in no table: closed between the descriptor read and the table read.
    pub unlisted: Vec<u64>,
}

/// The inode of a descriptor's link target `socket:[INODE]`.
pub fn inode(target: &str) -> Option<u64> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// The tables of the process whose `/proc/<pid>/net` is `net`, by name; a table that cannot be
/// read (the kernel lacks it, or the process is gone) is empty.
pub fn tables(net: &Path) -> Vec<(String, String)> {
    TABLES
        .iter()
        .map(|table| {
            (
                (*table).to_string(),
                std::fs::read_to_string(net.join(table)).unwrap_or_default(),
            )
        })
        .collect()
}

/// One table row of `inode`: the row's columns, keyed by the header's names. The inet tables'
/// headers name `tx_queue rx_queue` and `tr tm->when` where each row has one `a:b` column, so
/// those pairs are one column each.
fn row(text: &str, inode: u64) -> Option<Vec<(String, String)>> {
    let mut lines = text.lines();
    let mut names: Vec<String> = Vec::new();
    for name in lines.next()?.split_whitespace() {
        match name {
            "rx_queue" | "tm->when" => {
                if let Some(last) = names.last_mut() {
                    last.push(' ');
                    last.push_str(name);
                }
            }
            _ => names.push(name.to_ascii_lowercase()),
        }
    }
    let at = names.iter().position(|name| name == "inode")?;
    let wanted = inode.to_string();
    lines.find_map(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.get(at) != Some(&wanted.as_str()) {
            return None;
        }
        let mut row: Vec<(String, String)> = names
            .iter()
            .zip(&columns)
            .map(|(name, value)| (name.clone(), (*value).to_string()))
            .collect();
        // A unix socket's path may hold spaces: it is the rest of the row.
        if names.last().is_some_and(|name| name == "path") && columns.len() > names.len() {
            let rest = columns[names.len() - 1..].join(" ");
            if let Some(last) = row.last_mut() {
                last.1 = rest;
            }
        }
        Some(row)
    })
}

fn column<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    row.iter()
        .find(|(column, _)| column == name)
        .map_or("", |(_, value)| value.as_str())
}

/// Judge each of `inodes` by the first of `tables` (name and text, as [`tables`] reads them)
/// that lists it: an unnamed `SOCK_SEQPACKET` unix socket is the standard library's spawn
/// plumbing and is ignored; a unix socket with a path (a listener, or a connection to a named
/// endpoint) is a violation; an unnamed stream or datagram unix socket is a violation when
/// connected (a client of a named server shows no path of its own); every inet, packet or
/// netlink socket is a violation. An inode no table lists is returned as unlisted.
pub fn judge(tables: &[(String, String)], inodes: &[u64]) -> Judged {
    let mut judged = Judged::default();
    for &inode in inodes {
        let Some((table, row)) = tables
            .iter()
            .find_map(|(table, text)| row(text, inode).map(|row| (table.as_str(), row)))
        else {
            judged.unlisted.push(inode);
            continue;
        };
        let violation = |kind: &str, state: &str, path: String| Violation {
            inode,
            table: table.to_string(),
            kind: kind.to_string(),
            state: state.to_string(),
            path,
        };
        let found = match table {
            "unix" => {
                let (kind, state, path) = (
                    column(&row, "type"),
                    column(&row, "st"),
                    column(&row, "path"),
                );
                if !path.is_empty() || (kind != SEQPACKET && state == CONNECTED) {
                    Some(violation(kind, state, path.to_string()))
                } else {
                    None
                }
            }
            "packet" => Some(violation(column(&row, "type"), "-", String::new())),
            "netlink" => Some(violation(column(&row, "eth"), "-", String::new())),
            _ => Some(violation(
                table,
                column(&row, "st"),
                format!(
                    "{} -> {}",
                    column(&row, "local_address"),
                    // The inet6 tables name the remote address in full.
                    match column(&row, "rem_address") {
                        "" => column(&row, "remote_address"),
                        remote => remote,
                    }
                ),
            )),
        };
        judged.violations.extend(found);
    }
    judged
}

/// Every listening or connected socket among `inodes` of the process whose `/proc/<pid>/net`
/// is `net`: the tables are read, and once more for the inodes they did not list; an inode
/// still unlisted was closed in between and is ignored.
pub fn violations(net: &Path, inodes: &[u64]) -> Vec<Violation> {
    if inodes.is_empty() {
        return Vec::new();
    }
    let first = judge(&tables(net), inodes);
    let mut violations = first.violations;
    if !first.unlisted.is_empty() {
        violations.extend(judge(&tables(net), &first.unlisted).violations);
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIX: &str = "\
Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000003 00000000 00000000 0005 03 104248
0000000000000000: 00000003 00000000 00000000 0005 03 104249
0000000000000000: 00000002 00000000 00010000 0001 01 2001 /run/af test.sock
0000000000000000: 00000003 00000000 00000000 0001 03 2002
0000000000000000: 00000002 00000000 00000000 0002 01 2003
0000000000000000: 00000002 00000000 00010000 0001 01 2004 @abstract
";
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0CEA 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 3001 1 0000000000000000 100 0 0 10 0
";
    const UDP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  1: 00000000000000000000000000000000:14E9 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000   501        0 3002 2 0000000000000000 0
";
    const NETLINK: &str =
        "sk               Eth Pid        Groups   Rmem     Wmem     Dump  Locks    Drops    Inode
0000000000000000 0   4026       00000000 0        0        0     2        0        4001
";
    const PACKET: &str = "sk               RefCnt Type Proto  Iface R Rmem   User   Inode
0000000000000000 3      3    0003   2     1 0      0      5001
";

    fn tables() -> Vec<(String, String)> {
        let text = |name: &str| match name {
            "unix" => UNIX,
            "tcp" => TCP,
            "udp6" => UDP6,
            "netlink" => NETLINK,
            "packet" => PACKET,
            _ => "",
        };
        TABLES
            .iter()
            .map(|table| ((*table).to_string(), text(table).to_string()))
            .collect()
    }

    fn one(inode: u64) -> Judged {
        judge(&tables(), &[inode])
    }

    #[test]
    fn socket_inode_of_a_descriptor_target() {
        assert_eq!(inode("socket:[104248]"), Some(104248));
        assert_eq!(inode("pipe:[104248]"), None);
        assert_eq!(inode("/dev/null"), None);
    }

    #[test]
    fn socket_unnamed_seqpacket_pair_is_spawn_plumbing() {
        assert_eq!(judge(&tables(), &[104248, 104249]), Judged::default());
    }

    #[test]
    fn socket_named_unix_listener_is_a_violation() {
        let judged = one(2001);
        assert_eq!(
            judged.violations,
            [Violation {
                inode: 2001,
                table: "unix".into(),
                kind: "0001".into(),
                state: "01".into(),
                path: "/run/af test.sock".into(),
            }]
        );
        assert_eq!(one(2004).violations[0].path, "@abstract");
    }

    #[test]
    fn socket_connected_unnamed_stream_client_is_a_violation() {
        let judged = one(2002);
        assert_eq!(judged.violations.len(), 1, "{judged:?}");
        assert_eq!(
            (
                judged.violations[0].kind.as_str(),
                judged.violations[0].state.as_str()
            ),
            ("0001", "03")
        );
        // An unconnected unnamed datagram socket neither listens nor connects.
        assert_eq!(one(2003), Judged::default());
    }

    #[test]
    fn socket_tcp_listener_and_udp_are_violations() {
        let judged = one(3001);
        assert_eq!(
            judged.violations,
            [Violation {
                inode: 3001,
                table: "tcp".into(),
                kind: "tcp".into(),
                state: "0A".into(),
                path: "0100007F:0CEA -> 00000000:0000".into(),
            }]
        );
        let judged = one(3002);
        assert_eq!(judged.violations.len(), 1, "{judged:?}");
        assert_eq!(judged.violations[0].table, "udp6");
        assert_eq!(judged.violations[0].state, "07");
        assert_eq!(
            judged.violations[0].path,
            "00000000000000000000000000000000:14E9 -> 00000000000000000000000000000000:0000"
        );
    }

    #[test]
    fn socket_netlink_and_packet_are_violations() {
        let netlink = one(4001);
        assert_eq!(netlink.violations.len(), 1, "{netlink:?}");
        assert_eq!(
            (
                netlink.violations[0].table.as_str(),
                netlink.violations[0].kind.as_str()
            ),
            ("netlink", "0")
        );
        let packet = one(5001);
        assert_eq!(packet.violations.len(), 1, "{packet:?}");
        assert_eq!(packet.violations[0].table, "packet");
        let shown = packet.violations[0].to_string();
        assert!(
            shown.contains("5001") && shown.contains("packet"),
            "{shown}"
        );
    }

    #[test]
    fn socket_in_no_table_is_unlisted() {
        let judged = judge(&tables(), &[104248, 9999, 2001]);
        assert_eq!(judged.unlisted, [9999]);
        assert_eq!(judged.violations.len(), 1);
        // A process whose tables cannot be read lists nothing.
        let missing = Path::new("/nonexistent/proc/net");
        assert!(violations(missing, &[9999]).is_empty());
        assert!(violations(missing, &[]).is_empty());
    }
}
