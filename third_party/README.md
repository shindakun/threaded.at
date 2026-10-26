# third_party

`jacquard-common` and `jacquard-oauth` 0.12.2 from crates.io, patched to use
the RustCrypto 0.14 curves. Upstream 0.12.2 pins `p256`, `p384`, and `k256`
0.13, which depend on `crypto-bigint` 0.5.5. RUSTSEC-2026-0387 reports
timing variability in its constant-time operations; the fix is in
`crypto-bigint` 0.7.6. These copies pull in 0.7.9.

`Cargo.toml` at the repository root points both crates here with
`[patch.crates-io]`.

| Crate | Change |
|---|---|
| `jacquard-common` | `k256` and `p256` 0.14, `signature` 3 |
| `jacquard-oauth` | `elliptic-curve`, `p256`, `p384`, `k256` 0.14; `jose-jwk` without its `crypto` feature, which pins `p256` 0.13 |
| `jacquard-oauth` | `src/ec.rs` replaces jose-jwk's P-256 conversions; `utils.rs`, `dpop.rs`, `keyset.rs`, and one test in `atproto.rs` call it; `SigningKey::from_slice` replaces `from_bytes` |

Both crates are MPL-2.0; `LICENSE` in each directory is the upstream license.
Their test suites pass: `cargo test --manifest-path
third_party/jacquard-oauth/Cargo.toml --lib` and the same for
`jacquard-common`.

Remove both directories and the `[patch.crates-io]` table once a jacquard
release depends on `crypto-bigint` 0.7.6 or later. `cargo tree -i
crypto-bigint` shows which version is in the build.
