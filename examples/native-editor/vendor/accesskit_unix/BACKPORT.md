# AccessKit Unix compatibility backports

This is the published `accesskit_unix` 0.21.1 source with two upstream changes:
`src/context.rs` watches `org.a11y.Status.IsEnabled` instead of the removed
`ScreenReaderEnabled` property. The new property activates native accessibility
with current AT-SPI while also working with older services.
`src/atspi/bus.rs` retains the desktop reference returned by AT-SPI embedding,
and `src/atspi/interfaces/accessible.rs` exposes it as the application's parent.
This lets current Orca recognize ordinary native window and focus events.

The source comes from AccessKit commit
[`f40dfc01a0c0e76de535969f82fb35e19513737d`](https://github.com/AccessKit/accesskit/tree/f40dfc01a0c0e76de535969f82fb35e19513737d/platforms/unix).
The activation change is upstream
[#715](https://github.com/AccessKit/accesskit/pull/715), commit
[`e7299a753d78e8b00dd75e1d2182abb517648f98`](https://github.com/AccessKit/accesskit/commit/e7299a753d78e8b00dd75e1d2182abb517648f98).
The desktop-parent change is upstream
[#740](https://github.com/AccessKit/accesskit/pull/740), commit
[`e1f63acbb2c36e3cae741871300aeb121c9e6274`](https://github.com/AccessKit/accesskit/commit/e1f63acbb2c36e3cae741871300aeb121c9e6274).
Version 0.21.1 has no cache interface or other consumer of this reference, so
the parent backport stores the returned address directly on the root interface
instead of introducing the shared `Arc<OnceLock<_>>` used by newer upstream code.
All other Rust source and the normalized registry manifest remain unchanged.
The original MIT and Apache-2.0 licenses are included alongside this file.

The patch belongs only to this repository example's Cargo workspace. It retains
the AccessKit 0.24 and AT-SPI-common 0.18.1 types used by egui-winit 0.36.2,
without changing the library's dependency graph. Remove it when a compatible
upstream release includes both fixes. Native sessions on current and older
AT-SPI services should verify activation, ordinary screen-reader focus,
narration, and selection actions when updating this source. This backport does
not add the newer upstream `EditableText` interface.
