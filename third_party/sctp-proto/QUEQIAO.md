# sctp-proto 0.10.8, patched for Queqiao

The published crate (crates.io, MIT OR Apache-2.0: LICENSE-MIT, LICENSE-APACHE), source unchanged but for:

- `src/config.rs`: `set_default_rto_ms(initial, min, max)`, which sets the retransmission timeouts that
  `TransportConfig::default()` starts from (RFC 4960's 3000/1000/60000 ms before). str0m builds its SCTP transport
  configuration from the defaults and offers no setting of its own; Queqiao sets 1000/250/3000 ms
  (`queqiao-transport`, `SCTP_RTO_MS`), because the 1 s minimum stalled file transfers for a second after each
  lost burst tail.
- `src/lib.rs`: exports it.
- `Cargo.toml`: the test target and dev-dependencies removed (the tests folder is not copied).

Used through `[patch.crates-io]` in the workspace's Cargo.toml. To drop it: remove that entry and this folder,
and the `set_default_rto_ms` call.
