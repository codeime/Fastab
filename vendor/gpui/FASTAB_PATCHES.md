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
- Four tests use real Metal allocations and command completion to cover late
  returns, reopening after buffer growth, and another renderer surviving a close.
  A shared-event gate keeps an already committed command in flight
  while its renderer is dropped; releasing the gate verifies the GPU result and
  prevents a late return from repopulating the retired pool.

Fastab also changes `src/platform/mac/window.rs`:

- Logical window destruction takes the renderer and callbacks out of window
  state, stops frame delivery, detaches the native view's layer, and drops GPU
  resources without holding the state mutex. AppKit can retain a closed view;
  GPU teardown must not wait for that view's `dealloc`.
- Closed views reject late native callbacks. Callbacks and input handlers taken
  out before a close are not restored afterward, and synthetic drag loops exit
  when their window closes or its state disappears. Reusable hidden windows
  keep their renderer until they are actually destroyed.
- Layer/view cleanup can synchronously reenter AppKit. The closed state is set
  first, superclass frame-size changes happen outside the state mutex, and the
  detached view is retained through deferred native close so a callback that
  closes its own window cannot free its receiver midway through execution.
- The spurious `windowDidBecomeKey:` workaround retains the native window and
  releases the window-state mutex before calling `resignKeyWindow`. AppKit can
  synchronously deliver `windowDidResignKey:` from that call, which reenters the
  same callback and otherwise deadlocks on the mutex. The retained window stays
  alive through the call; normal activation and frame-request paths are unchanged.

Additional lifecycle fixes:

- `display_link.rs` reuses one process-lifetime CoreVideo clock per encountered
  display. This follows the lifetime constraint documented in upstream
  [#32116](https://github.com/zed-industries/zed/pull/32116) and
  [#60696](https://github.com/zed-industries/zed/pull/60696): stopping a link does
  not join its IO thread, so releasing each retired link can race that thread.
  Window subscriptions are removed under the producer's lock before their GCD
  sources are cancelled and released. The source finalizer owns the callback
  context until handlers finish. Start/stop are idempotent; CoreVideo calls
  happen outside the registry lock. Five tests exercise real GCD finalization,
  callback self-close and production registry failure/retry behavior. The unused
  `dispatch_suspend` binding is removed from `build.rs`.
- `metal_atlas.rs` removes the key mapping before decrementing its texture's
  reference count, so repeated removal cannot free another key's texture and a
  later lookup rebuilds removed content. Three real Metal tests cover shared
  textures, repeated slot reuse, and committed GPU reads surviving deletion and
  replacement. Individual tile regions are not reused during a texture's life.
- `window.rs` autoreleases native attributed-substring results and checks nullable
  text-query output pointers. Two Foundation tests exercise UTF-16 output and
  autorelease-pool ownership. Screen-number strings in `window.rs`, `display.rs`
  and `screen_capture.rs`, plus tabbing identifiers, use the existing autoreleased
  string helper.

The root `[patch.crates-io]` applies this copy to all GPUI consumers. Other
`gpui_*` crates retain their registry dependencies. Font identities, text caches,
rendering primitives and buffer allocation policy are unchanged.

This does not unregister private AppKit notification observers or promise that
all native views and application-wide caches disappear on close. It separates
their lifetime from the resources needed only by a live window.

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
