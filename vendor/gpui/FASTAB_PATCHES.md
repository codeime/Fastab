# GPUI vendoring

This directory contains the crates.io `gpui` 0.2.2 release, licensed under
Apache-2.0 (see `LICENSE-APACHE`). The original published, normalized
`Cargo.toml`, source, build inputs, examples and tests are retained. Cargo's
`.cargo-ok`, `.cargo_vcs_info.json` and nested `Cargo.lock` are omitted.

Provenance:

- Crate: <https://crates.io/crates/gpui/0.2.2>
- Published archive SHA-256:
  `979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`
- Upstream: <https://github.com/zed-industries/zed>
- Published VCS metadata: commit
  `69e2130295c2649963eb639fc70b4f2ee8ea1624`, path `crates/gpui`, `dirty: true`.
  The release archive, rather than that Git commit alone, is the baseline.

Fastab changes `src/platform/mac/metal_renderer.rs`:

- Destroying a Metal renderer trims the application-wide idle instance-buffer
  pool. The configured buffer size is preserved, including prior growth.
- Each acquired buffer records a pool generation. GPU completion returns a
  buffer to the pool only if its generation and size still match. A renderer
  closing therefore cannot be undone by a late completion, while outstanding
  buffers remain owned until their GPU work finishes.
- Retired Metal buffers are returned from pool operations and dropped after
  releasing the pool mutex, including the existing buffer-growth reset path.
- Three tests use real Metal allocations and command completion to cover late
  returns, reopening after buffer growth, and another renderer surviving a close.

The root `[patch.crates-io]` applies this copy to all GPUI consumers. Other
`gpui_*` crates retain their registry dependencies. Font identities, text caches,
rendering primitives and buffer allocation policy are unchanged.

Run the hardware tests on a Metal-capable macOS host from a temporary verification
copy of this repository. In that copy only, add `"vendor/gpui"` to the root
`workspace.members`, then run:

```sh
cargo test -p gpui --features runtime_shaders,test-support --lib instance_buffer_pool_tests -- --ignored --test-threads=1
```

Keep that workspace-members edit out of the delivered repository. Cargo cannot
test this dependency outside the workspace because GPUI has its own
dev-dependencies; selecting `fastab_gpui` alongside it does not remove this
restriction. `runtime_shaders` matches Fastab's renderer build, and
`test-support` enables the supporting crates' test features needed by GPUI's
library test harness. No change to GPUI's published manifest is required.

These tests intentionally require an explicit run because headless macOS CI
hosts may lack a Metal device. Also validate closing/reopening Fastab Settings
while using autocomplete, and measure the installed app's idle physical
footprint; the unit tests do not establish UI behavior or memory savings.
