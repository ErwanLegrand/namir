//! FR-ERR-060 (Must, `Verify:` **S**): "In 1.0, Namir shall make no outbound network connection and
//! shall transmit no data off the user's machine: no telemetry, no crash-report upload, no update
//! check. *Verify:* S — a build-time check that no network-capable dependency is linked into the 1.0
//! binaries." Also NFR-SEC-030, which restates the same prohibition from the security side.
//!
//! # The half `deny.toml` cannot cover
//!
//! `deny.toml`'s `[bans] deny` list is the *dependency* half, and its own comment says it "is
//! deliberately not exhaustive of every crate that could conceivably open a socket". That is
//! honest, and it is also not the gap this module closes. The gap is one level closer to home:
//! **a first-party crate needs no dependency at all to open a socket.** `std::net::TcpStream` is in
//! the standard library, so a line of Namir's own code could open an outbound connection with
//! `Cargo.lock` untouched, `cargo deny check bans` green, and `THIRD-PARTY-NOTICES.md` unchanged —
//! every existing control passing while the requirement is violated.
//!
//! This module is that check: no `.rs` file under `crates/` may name any of [`NETWORK_APIS`].
//!
//! **`xtask` is deliberately outside the scanned set.** FR-ERR-060's subject is "the 1.0 binaries",
//! and `xtask` is dev tooling in neither product's dependency graph (see its own `Cargo.toml`). A
//! socket in `xtask` would be a different problem — a supply-chain one — and pretending this check
//! covers it would misdescribe what it asserts.
//!
//! # Residual blind spots, stated rather than pretended closed
//!
//! - **Line-based**, like every other scanner in this crate, and comment lines are skipped. Prose
//!   about networking is everywhere in this repository — this file, `deny.toml`, FR-ERR-070's whole
//!   sub-clause list — and a check that tripped on the prohibition's own statement would be
//!   uninstallable.
//! - **Names, not reachability.** A first-party crate calling a *dependency* that opens a socket
//!   names nothing here; that is `deny.toml`'s half, and it is by-name and non-exhaustive. The two
//!   halves together are what FR-ERR-060's partial still records.
//! - **Nothing sees a raw syscall or a `libc` socket call.** Neither is reachable in this workspace
//!   — D-5.3 confines `unsafe` to three named files, none of which is a network module — but the
//!   limit is real rather than argued away.

/// Standard-library networking names, matched as whole identifiers. Every one of them is
/// unambiguous: none has an ordinary non-networking meaning, and none appears anywhere under
/// `crates/` today.
///
/// `IpAddr`/`SocketAddr` and friends open no connection by themselves, and are on the list anyway.
/// A first-party crate that has reason to name an IP address is a first-party crate someone should
/// look at, which is exactly what a failing static check causes.
pub const NETWORK_APIS: &[&str] = &[
    "std::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "SocketAddr",
    "SocketAddrV4",
    "SocketAddrV6",
    "ToSocketAddrs",
    "IpAddr",
    "Ipv4Addr",
    "Ipv6Addr",
];

/// Whether `line` names `ident` as a whole identifier. `std::net` contains `::`, which is not an
/// identifier byte, so the same boundary rule serves both the path and the type names.
fn names_identifier(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(ident).any(|(start, _)| {
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Scans `source` for [`NETWORK_APIS`], line by line, skipping lines whose trimmed form begins
/// `//`. Returns `(1-indexed line number, matched name)` per occurrence, in [`NETWORK_APIS`] order.
/// Pure string logic so it is unit-testable without a filesystem; [`crate::main`] applies it to the
/// real files.
pub fn scan_network_apis(source: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for name in NETWORK_APIS {
            if names_identifier(line, name) {
                hits.push((idx + 1, *name));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outbound_connection_is_flagged_however_it_is_spelled() {
        assert_eq!(
            scan_network_apis("let s = std::net::TcpStream::connect(addr)?;\n"),
            vec![(1, "std::net"), (1, "TcpStream")]
        );
        assert_eq!(
            scan_network_apis("use std::net::UdpSocket;\n"),
            vec![(1, "std::net"), (1, "UdpSocket")]
        );
        // The import broken from the call, which is how a line scanner would otherwise miss the
        // call site entirely.
        assert_eq!(
            scan_network_apis("use std::net::TcpStream as Conn;\nlet c = Conn::connect(a)?;\n"),
            vec![(1, "std::net"), (1, "TcpStream")]
        );
    }

    #[test]
    fn an_address_type_is_flagged_even_though_it_opens_nothing() {
        assert_eq!(
            scan_network_apis("fn parse(s: &str) -> Option<SocketAddr> { None }\n"),
            vec![(1, "SocketAddr")]
        );
    }

    #[test]
    fn ordinary_source_is_clean() {
        let source = "use std::path::PathBuf;\nlet net_gain_db = 3.0;\nstruct IpAddress;\n\
                      fn socket_addressing() {}\n";
        assert!(scan_network_apis(source).is_empty());
    }

    #[test]
    fn prose_about_the_prohibition_is_not_flagged() {
        // Load-bearing: this module's own doc comment names every item on the list, and FR-ERR-060
        // is discussed at length across the governing documents and `deny.toml`.
        let source = "// FR-ERR-060: never open a std::net::TcpStream from here.\n\
                      /// See `UdpSocket` for what this does not do.\nlet x = 1;\n";
        assert!(scan_network_apis(source).is_empty());
    }

    #[test]
    fn the_list_is_non_empty_and_carries_no_ambiguous_word() {
        assert!(!NETWORK_APIS.is_empty());
        for name in NETWORK_APIS {
            assert!(!name.is_empty());
            // Every entry is either a path or a CamelCase type: nothing on this list can collide
            // with an ordinary lower-case identifier the way `windows`/`unix` do in `layering`.
            assert!(
                name.contains("::") || name.starts_with(|c: char| c.is_ascii_uppercase()),
                "{name}"
            );
        }
    }
}
