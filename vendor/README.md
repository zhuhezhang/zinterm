# Vendored crates

## russh

Patched [russh](https://github.com/warp-tech/russh) **0.52.1** (Apache-2.0).

Upstream does not implement `hmac-md5`. This tree adds it behind the `md5-mac`
feature (RustCrypto `md-5`, dependency renamed to `md5_digest` to avoid clashing
with russh's existing `md5` 0.7 PKCS#5 helper).

`GexParams` also allows a 1024-bit minimum group size (upstream floors at 2048)
and defaults to OpenSSH-like 2048/3072/8192 so DH-GEX works with Maipu / H3C /
older VRP switches that still offer small moduli.

SSH keepalive replies follow OpenSSH / PuTTY / MobaXterm: global
`keepalive@openssh.com` gets `REQUEST_FAILURE` (82) instead of upstream's
`REQUEST_SUCCESS` (81), and channel keepalives fall through to
`CHANNEL_FAILURE`. Some appliances (e.g. Hillstone SG-6000 / StoneOS 5.x)
only treat FAILURE as a ClientAlive acknowledgement; SUCCESS leads to
`UNIMPLEMENTED` or unanswered probes and idle TCP disconnect after
`ClientAliveCountMax`.

When upgrading russh, re-apply the `src/mac/mod.rs` / `Cargo.toml` /
`GexParams` / `src/client/encrypted.rs` (keepalive reply) deltas or drop the
vendor patch if upstream gains HMAC-MD5, softer GEX bounds, and OpenSSH-like
keepalive FAILURE replies.
