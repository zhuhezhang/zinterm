# Vendored crates

## russh

Patched [russh](https://github.com/warp-tech/russh) **0.52.1** (Apache-2.0).

Upstream does not implement `hmac-md5`. This tree adds it behind the `md5-mac`
feature (RustCrypto `md-5`, dependency renamed to `md5_digest` to avoid clashing
with russh's existing `md5` 0.7 PKCS#5 helper).

When upgrading russh, re-apply the `src/mac/mod.rs` / `Cargo.toml` delta or drop
the vendor patch if upstream gains HMAC-MD5.
