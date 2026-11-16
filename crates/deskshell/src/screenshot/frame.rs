//! The selector's frames: the flow's captures, swizzled from BGRA to RGBA off the UI thread.

use deskshell_core::screenshot::pixels;
use deskshell_ui::image::{Error, Image};
use deskshell_wl::shot;
use iced::widget::image::Handle;

pub fn image(frame: &shot::Frame) -> Result<Image, Error> {
    let view = pixels::Frame::new(
        frame.width,
        frame.height,
        frame.stride as usize,
        &frame.pixels,
    )
    .ok_or_else(|| Error::Decode(String::from("the frame is shorter than its size")))?;
    let rgba = pixels::to_rgba(view);
    Ok(Image {
        handle: Handle::from_rgba(frame.width, frame.height, rgba),
        width: frame.width,
        height: frame.height,
    })
}
