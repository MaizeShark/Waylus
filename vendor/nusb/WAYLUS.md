# nusb 0.2.9, patched

Copy of [nusb](https://github.com/kevinmehall/nusb) 0.2.9 from crates.io (MIT or Apache-2.0, see
`LICENSE-MIT`, `LICENSE-APACHE`), used through `[patch.crates-io]` in the workspace
`Cargo.toml` (the crate files without its `.github`, `.cargo`, `Cargo.lock` and
`Cargo.toml.orig`). It is vendored because upstream nusb can't use the tablet's USB accessory
interface on Windows when Samsung's Android USB driver is installed. Both changes are
Windows only and marked `Waylus patch` in the source; the Linux and macOS code is unchanged.

**1. Vendor composite drivers** (`src/platform/windows_winusb/device.rs`, `claim_interface`;
commit 68ccdb8). nusb opens an interface of a composite device only when the parent
device's driver is Microsoft's `usbccgp`. With Samsung's USB driver installed (common, it
is the usual "Android USB driver" download), the parent of an Android device in accessory
mode (18d1:2d01) gets Samsung's composite driver `dg_ssudbus` instead, and nusb refuses
with "incompatible driver is installed for this device" although the accessory interface
itself is bound to WinUSB. The patch treats any parent that has a child for the requested
interface (hardware ID `&MI_xx`) like `usbccgp`; the child must still be WinUSB, as before.

**2. Aborting pipes** (`src/platform/windows_winusb/device.rs` and `src/device.rs`;
commit 5c42e16). The same driver never completes a transfer cancelled with `CancelIoEx`,
and Windows uses that path when a process exits too, so a process with bulk reads
outstanding stayed half-terminated (one thread, no handles, the .exe locked) until the
tablet was unplugged. `cancel_all` now also calls `WinUsb_AbortPipe` when transfers are
still pending, and a new public `Interface::abort_pipe` (Windows only) lets Waylus abort
its accessory pipes right before it exits.

Drop this copy (and the `[patch.crates-io]` entry in the workspace `Cargo.toml`) once
upstream nusb accepts vendor composite drivers and aborts pending transfers itself.
