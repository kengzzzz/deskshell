# Vendored crates

## iced_wgpu 0.14.0

A copy of the crates.io release with one change, kept in `iced_wgpu.patch`. The root `Cargo.toml` swaps it in through `[patch.crates-io]`, so iced and `iced_exwlshell` build against it.

Why: iced_wgpu opens its wgpu device with `Adapter::request_device` and no way to add Vulkan extensions. Hyprland on NVIDIA accepts only block-linear dmabufs for capture, and importing those into the shell's device needs `VK_EXT_image_drm_format_modifier`, which wgpu 27 never enables (decision 9 in `docs/plan.md`).

The change: `iced_wgpu::window::compositor::set_device_hook` registers a function that opens the device in place of `request_device`. `deskshell-wl` uses it to open the device through wgpu-hal's `open_with_callback` with the extra extensions. A hook that returns `None` leaves the upstream behaviour.

Drop the copy when iced moves to a wgpu release that enables the extension itself (wgpu-hal 30 does) and lets the app request the matching feature.

To update: copy the new release from the cargo registry, apply `iced_wgpu.patch`, and add the `LICENSE` file (MIT, from the iced repository at the release tag).
