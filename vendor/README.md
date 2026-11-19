# Vendored crates

## iced_wgpu 0.14.0

A copy of the crates.io release with two changes, kept in `iced_wgpu.patch`. The root `Cargo.toml` swaps it in through `[patch.crates-io]`, so iced and `iced_exwlshell` build against it.

### Device hook

Why: iced_wgpu opens its wgpu device with `Adapter::request_device` and no way to add Vulkan extensions. Hyprland on NVIDIA accepts only block-linear dmabufs for capture, and importing those into the shell's device needs `VK_EXT_image_drm_format_modifier`, which wgpu 27 never enables (decision 9 in `docs/plan.md`).

The change: `iced_wgpu::window::compositor::set_device_hook` registers a function that opens the device in place of `request_device`. `deskshell-wl` uses it to open the device through wgpu-hal's `open_with_callback` with the extra extensions. A hook that returns `None` leaves the upstream behaviour.

Drop this change when iced moves to a wgpu release that enables the extension itself (wgpu-hal 30 does) and lets the app request the matching feature.

### Image uploads on the calling thread

Why: upstream decodes non-RGBA image handles, and uploads RGBA images of 2 MiB and more, on a worker thread that calls `Queue::submit`. wgpu 27 drops and reconfigures a surface through `vkDeviceWaitIdle` without the lock that guards `vkQueueSubmit`, so a submit from a second thread breaks Vulkan's host synchronization rules. deskshell keeps every submit on the presenting thread (`docs/specs/wl.md`, "GPU submits").

The change: the worker's jobs run on the calling thread, which is the presenting thread. Results still go through the worker's channel and reach the cache on the next draw, so redraws and allocation callbacks behave as upstream. The channel is unbounded, because the thread that sends now also receives. Large or non-RGBA images now decode and upload in the frame that first draws them.

Drop this change when wgpu takes the queue lock in surface drop and configure, or iced stops submitting from its worker.

Drop the copy when neither change is needed.

To update: copy the new release from the cargo registry, apply `iced_wgpu.patch`, and add the `LICENSE` file (MIT, from the iced repository at the release tag).

## exwlshellev 0.21.4

A copy of the crates.io release with one change, kept in `exwlshellev.patch`. The root `Cargo.toml` swaps it in through `[patch.crates-io]`, so iced_exwlshell builds against it.

Why: when a shell closes, exwlshellev hands `Closed` to iced_exwlshell before it destroys the layer. iced_exwlshell drops the wgpu surface there, and wgpu waits in `vkDeviceWaitIdle` with no limit. On 2026-11-19 that wait never returned on NVIDIA, and the invisible share picker stayed mapped and took every click.

The change: right before `Closed`, a layer's `zwlr_layer_surface_v1` role is destroyed and the connection flushed. The compositor unmaps it, so a close that hangs leaves nothing on screen and nothing taking input. The `wl_surface` stays alive until after the wgpu surface is gone, as Vulkan's Wayland surface rules require. A null buffer would unmap too, but Hyprland takes its fade-out snapshot at unmap, and with the buffer already gone the panel would vanish without a fade.

Drop the copy when an exwlshellev release unmaps before `Closed` itself.

To update: copy `Cargo.toml`, `README.md` and `src` of the new release from the cargo registry, apply `exwlshellev.patch`, and add the `LICENSE` file (MIT, from waycrate/exwlshelleventloop at the release tag).
