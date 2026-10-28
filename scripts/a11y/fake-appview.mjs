// Plays the Bluesky AppView for the sign-in typeahead, so the browser
// checks run without the network: "al..." finds two accounts, anything
// else finds none.
//
// usage: node fake-appview.mjs PORT
import { createServer } from "node:http";

const port = Number(process.argv[2]);
createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (url.pathname !== "/xrpc/app.bsky.actor.searchActorsTypeahead") {
    res.writeHead(404).end();
    return;
  }
  const q = url.searchParams.get("q") || "";
  const actors = q.startsWith("al")
    ? [
        { did: "did:plc:alice", handle: "alice.example", displayName: "Alice" },
        { did: "did:plc:alfie", handle: "alfie.example" },
      ]
    : [];
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ actors }));
}).listen(port, "127.0.0.1");
