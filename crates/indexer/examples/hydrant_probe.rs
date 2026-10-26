//! Calls every client method against a running hydrant and prints what came
//! back, to check the client against the real API.
//!
//! usage: hydrant_probe SOCKET [DID]

use threaded_indexer::client::{Event, Hydrant};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket = args.next().ok_or("usage: hydrant_probe SOCKET [DID]")?;
    let did = args.next();
    let h = Hydrant::new(&socket);

    let head = h.stream_head().await?;
    println!("head: {head:?}");
    if let Some(d) = &did {
        println!("repo: {:?}", h.repo(d).await?);
        println!("track again: {:?}", h.track(&[d]).await?);
    }
    println!(
        "unknown repo: {:?}",
        h.repo("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa").await?
    );

    let mut s = h.stream(Some(0)).await?;
    while let Ok(Some(next)) =
        tokio::time::timeout(std::time::Duration::from_secs(3), s.next()).await
    {
        match next? {
            Event::Record { id, record } => println!(
                "{id} {:?} {} live={} cid={} title={:?}",
                record.action,
                record.uri(),
                record.live,
                record.cid.is_some(),
                record
                    .record
                    .as_ref()
                    .and_then(|r| r.get("title").or_else(|| r.get("text"))),
            ),
            other => println!("{other:?}"),
        }
    }
    Ok(())
}
