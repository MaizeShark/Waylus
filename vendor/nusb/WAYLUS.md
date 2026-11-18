# nusb 0.2.9, patched

Unmodified copy of [nusb](https://github.com/kevinmehall/nusb) 0.2.9 (MIT or Apache-2.0, see
`LICENSE-MIT`, `LICENSE-APACHE`) with one change in `src/platform/windows_winusb/device.rs`,
`claim_interface`:

nusb opens an interface of a composite device only when the parent device's driver is
Microsoft's `usbccgp`. With Samsung's USB driver installed (common, it is the usual
"Android USB driver" download), the parent of an Android device in accessory mode
(18d1:2d01) gets Samsung's composite driver `dg_ssudbus` instead, and nusb refuses with
"incompatible driver is installed for this device" although the accessory interface
itself is bound to WinUSB. The patch treats any parent that has a child for the
requested interface (hardware ID `&MI_xx`) like `usbccgp`; the child must still be
WinUSB, as before.

Drop this copy (and the `[patch.crates-io]` entry in the workspace `Cargo.toml`) once
upstream nusb accepts vendor composite drivers.
