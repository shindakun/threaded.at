//! Reads Bluesky blocks: each account's `app.bsky.graph.block` records, from
//! its PDS, with the public `com.atproto.repo.listRecords`. An account's
//! blocks are read when it signs in, when its owner asks in settings, and
//! by a refresh in the background: hourly for accounts seen in the last 30
//! days, daily for the rest.
//!
//! A PDS address comes from a DID document, which its owner writes, so the
//! request is held to the public internet: https only, no redirects, no
//! address written as an IP, and a resolver that drops private, loopback,
//! and other local addresses.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use threaded_auth::client::Clients;
use threaded_store::{Block, Store};

const BLOCK: &str = "app.bsky.graph.block";
/// Records per page, the most `listRecords` returns.
const PAGE: usize = 100;
/// Enough for any person's own blocks; a repo past it keeps the first ones
/// read, and the rest are logged.
const MOST: usize = 20_000;
/// The largest response body read for one page.
const BODY_MAX: usize = 2 * 1024 * 1024;

pub const ACTIVE_EVERY: i64 = 3600;
pub const IDLE_EVERY: i64 = 86_400;
pub const ACTIVE_FOR: i64 = 30 * 86_400;

/// Blocks by DID, or the error a read of that DID meets.
pub type FixedBlocks = Arc<Mutex<HashMap<String, Result<Vec<Block>, String>>>>;

/// Where blocks are read from. Tests use `Fixed`.
#[derive(Clone)]
pub enum BlockReader {
    Network {
        clients: Arc<Clients>,
        http: reqwest::Client,
    },
    Fixed(FixedBlocks),
}

impl BlockReader {
    pub fn network(clients: Arc<Clients>) -> BlockReader {
        let http = reqwest::Client::builder()
            .https_only(true)
            // A proxy would make the connection, and the address checks
            // below would not apply to it.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .dns_resolver(Arc::new(PublicOnly))
            .user_agent("Threaded (https://threaded.at)")
            .build()
            .expect("a client with these settings builds");
        BlockReader::Network { clients, http }
    }

    /// Every block record in an account's repo.
    pub async fn read(&self, did: &str) -> Result<Vec<Block>, String> {
        match self {
            BlockReader::Fixed(map) => map
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(did)
                .cloned()
                .unwrap_or_else(|| Ok(Vec::new())),
            BlockReader::Network { clients, http } => {
                let pds = threaded_auth::flow::pds_of(clients, did)
                    .await
                    .ok_or("the DID document names no PDS")?;
                read_pds(http, &pds, did).await
            }
        }
    }
}

#[derive(Deserialize)]
struct Page {
    records: Vec<Record>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct Record {
    uri: String,
    value: serde_json::Value,
}

/// The base URL of a PDS, when it is one this module will ask: https, and
/// a host name rather than an IP address.
fn pds_base(pds: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(pds).map_err(|_| format!("{pds:?} is not a URL"))?;
    if url.scheme() != "https" {
        return Err(format!("{pds:?} is not https"));
    }
    let named = url.host_str().is_some_and(|h| {
        !h.starts_with('[') && h.parse::<IpAddr>().is_err() && !h.eq_ignore_ascii_case("localhost")
    });
    if named {
        Ok(url)
    } else {
        Err(format!("{pds:?} has no public host name"))
    }
}

async fn read_pds(http: &reqwest::Client, pds: &str, did: &str) -> Result<Vec<Block>, String> {
    let base = pds_base(pds)?;
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = base
            .join("/xrpc/com.atproto.repo.listRecords")
            .map_err(|e| e.to_string())?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("repo", did)
                .append_pair("collection", BLOCK)
                .append_pair("limit", &PAGE.to_string());
            if let Some(c) = &cursor {
                q.append_pair("cursor", c);
            }
        }
        let mut res = http.get(url).send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("{} answered {}", base, res.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > BODY_MAX {
                return Err("a page of records was too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        let page: Page = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let n = page.records.len();
        out.extend(page.records.iter().filter_map(|r| block_of(did, r)));
        if out.len() >= MOST {
            tracing::warn!(%did, "blocks: more than {MOST}; keeping the first");
            out.truncate(MOST);
            return Ok(out);
        }
        match page.cursor {
            Some(c) if n > 0 && cursor.as_ref() != Some(&c) => cursor = Some(c),
            _ => return Ok(out),
        }
    }
}

/// A block from a listed record: the record must be in this account's own
/// block collection, and its subject a DID.
fn block_of(did: &str, r: &Record) -> Option<Block> {
    let rkey = r
        .uri
        .strip_prefix("at://")?
        .strip_prefix(did)?
        .strip_prefix('/')?
        .strip_prefix(BLOCK)?
        .strip_prefix('/')?;
    let subject = r.value.get("subject")?.as_str()?;
    let ok_rkey = !rkey.is_empty() && rkey.len() <= 512 && !rkey.contains('/');
    let ok_subject = subject.starts_with("did:") && subject.len() <= 2048;
    (ok_rkey && ok_subject).then(|| Block {
        rkey: rkey.to_string(),
        subject_did: subject.to_string(),
    })
}

/// Reads an account's blocks and stores them. A failed read keeps the
/// blocks already held and is tried again later.
pub async fn refresh(store: &Store, reader: &BlockReader, did: &str, now: i64) -> bool {
    match reader.read(did).await {
        Ok(blocks) => match store.replace_blocks(did, &blocks, now).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(%did, error = %e, "blocks: storing");
                false
            }
        },
        Err(e) => {
            tracing::info!(%did, error = %e, "blocks: read failed");
            if let Err(e) = store.blocks_tried(did, now).await {
                tracing::warn!(%did, error = %e, "blocks: noting a failed read");
            }
            false
        }
    }
}

/// Reads the blocks of every account that is due, a few at a time, once a
/// minute, for as long as the server runs.
pub async fn run(store: Store, reader: BlockReader, clock: Arc<dyn Fn() -> i64 + Send + Sync>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let now = clock();
        let due = match store
            .blocks_due(now, now - ACTIVE_FOR, ACTIVE_EVERY, IDLE_EVERY, 30)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "blocks: finding accounts due");
                continue;
            }
        };
        for did in due {
            refresh(&store, &reader, &did, clock()).await;
        }
    }
}

/// Resolves host names to public addresses only, so a PDS address cannot
/// point the request into this machine or its network.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether an address is on the public internet.
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64
                // 6to4 and Teredo carry an IPv4 address, which can be a
                // local one.
                || s[0] == 0x2002
                || (s[0] == 0x2001 && s[1] == 0x0000))
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared, 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
        || o[0] >= 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses() {
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "240.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "2001:db8::1",
            "64:ff9b::7f00:1",
            "ff02::1",
            "2002:7f00:1::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn pds_addresses() {
        assert!(pds_base("https://inkcap.us-east.host.bsky.network").is_ok());
        assert!(pds_base("https://pds.example.com:8443/").is_ok());
        for bad in [
            "http://pds.example.com",
            "https://127.0.0.1",
            "https://[::1]",
            "https://localhost",
            "https://LOCALHOST:3000",
            "ftp://pds.example.com",
            "not a url",
        ] {
            assert!(pds_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn records_become_blocks_only_when_well_formed() {
        let did = "did:plc:alice";
        let rec = |uri: &str, subject: serde_json::Value| Record {
            uri: uri.to_string(),
            value: serde_json::json!({"$type": BLOCK, "subject": subject}),
        };
        assert_eq!(
            block_of(
                did,
                &rec(
                    "at://did:plc:alice/app.bsky.graph.block/3kb",
                    "did:plc:bob".into()
                )
            ),
            Some(Block {
                rkey: "3kb".into(),
                subject_did: "did:plc:bob".into()
            })
        );
        for (uri, subject) in [
            (
                "at://did:plc:mallory/app.bsky.graph.block/3kb",
                serde_json::json!("did:plc:bob"),
            ),
            (
                "at://did:plc:alice/app.bsky.graph.follow/3kb",
                serde_json::json!("did:plc:bob"),
            ),
            (
                "at://did:plc:alice/app.bsky.graph.block/",
                serde_json::json!("did:plc:bob"),
            ),
            (
                "at://did:plc:alice/app.bsky.graph.block/3kb",
                serde_json::json!("bob.example"),
            ),
            (
                "at://did:plc:alice/app.bsky.graph.block/3kb",
                serde_json::json!(7),
            ),
        ] {
            assert_eq!(block_of(did, &rec(uri, subject)), None, "{uri}");
        }
    }
}
