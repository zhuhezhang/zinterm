# Vendored crates

## russh

Patched [russh](https://github.com/warp-tech/russh) **0.52.1** (Apache-2.0).

Upstream does not implement `hmac-md5`. This tree adds it behind the `md5-mac`
feature (RustCrypto `md-5`, dependency renamed to `md5_digest` to avoid clashing
with russh's existing `md5` 0.7 PKCS#5 helper).

`GexParams` also allows a 1024-bit minimum group size (upstream floors at 2048)
and defaults to OpenSSH-like 2048/3072/8192 so DH-GEX works with Maipu / H3C /
older VRP switches that still offer small moduli.

When upgrading russh, re-apply the `src/mac/mod.rs` / `Cargo.toml` / `GexParams`
deltas or drop the vendor patch if upstream gains HMAC-MD5 and softer GEX bounds.
