# Local Bevy render patch

This directory vendors the published `bevy_render` 0.18.1 crate so AFC can
carry one narrow render hot-path fix.

- Crate: `bevy_render` 0.18.1
- crates.io checksum:
  `243523e33fe5dfcebc4240b1eb2fc16e855c5d4c0ea6a8393910740956770f44`
- Bevy source commit: `f667c282dad2c1419afb5836ded22a3ec263970e`
- Original licenses: MIT or Apache-2.0; both license files are retained.

## Backport

`PassSpanGuard::end` called `core::mem::forget(self)`. The guard owns its
diagnostic name as a `Cow<'static, str>`, and Bevy's shadow passes supply
dynamically owned names. A successful pass therefore leaked the owned string
on every rendered frame.

The local change replaces and drops `self.name` before forgetting the guard.
It intentionally leaves the guard's layout, successful-call behavior, and
panic-on-abandoned-guard contract unchanged.

Keep the `[patch.crates-io]` entry pinned to this directory until an equivalent
upstream release is adopted. When upgrading Bevy, remove this override only
after the graphical allocator scenarios show that live allocation growth has
not returned.
