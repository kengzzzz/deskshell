//! The native screenshot's PNG file from made-up frames, in a temp dir under `target/`.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use deskshell::screenshot::flow::{Frames, Picture};
use deskshell::screenshot::save;
use deskshell_core::screenshot::pixels::{Placement, Rect, Shot};
use deskshell_wl::shot;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "screenshot-save-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn frame(output: &str, width: u32, height: u32, blue: u8) -> shot::Frame {
    let stride = width * 4 + 16;
    let mut pixels = vec![0x55; stride as usize * height as usize];
    for y in 0..height {
        for x in 0..width {
            let at = (y * stride + x * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&[blue, y as u8, x as u8, 0]);
        }
    }
    shot::Frame {
        output: output.to_owned(),
        width,
        height,
        stride,
        format: shot::Format::Xrgb8888,
        pixels,
    }
}

fn decode(
    bytes: &[u8],
) -> (
    png::ColorType,
    png::BitDepth,
    u32,
    u32,
    Vec<u8>,
    usize,
) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut out = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut out).unwrap();
    out.truncate(info.buffer_size());
    let chunks = reader
        .info()
        .uncompressed_latin1_text
        .len()
        + reader
            .info()
            .compressed_latin1_text
            .len()
        + reader.info().utf8_text.len()
        + usize::from(reader.info().icc_profile.is_some())
        + usize::from(reader.info().srgb.is_some())
        + usize::from(reader.info().source_gamma.is_some())
        + usize::from(reader.info().pixel_dims.is_some());
    (
        info.color_type,
        info.bit_depth,
        info.width,
        info.height,
        out,
        chunks,
    )
}

#[test]
fn a_crop_round_trips_through_the_png_decoder() {
    let frames: Frames = Arc::from(vec![frame("Virtual-1", 64, 36, 7)]);
    let picture =
        Picture::Crop { frames, index: 0, rect: Rect { x: 10, y: 5, width: 20, height: 8 } };
    let shot = save::render(&picture, &[]).unwrap();
    assert_eq!((shot.width, shot.height), (20, 8));
    let png = save::encode(&shot).unwrap();
    let (color, depth, width, height, rgb, chunks) = decode(&png);
    assert_eq!(color, png::ColorType::Rgb, "no alpha");
    assert_eq!(depth, png::BitDepth::Eight);
    assert_eq!((width, height), (20, 8));
    assert_eq!(chunks, 0, "no ancillary chunks");
    for (y, row) in rgb
        .as_chunks::<{ 20 * 3 }>()
        .0
        .iter()
        .enumerate()
    {
        for (x, pixel) in row
            .as_chunks::<3>()
            .0
            .iter()
            .enumerate()
        {
            assert_eq!(
                *pixel,
                [10 + x as u8, 5 + y as u8, 7],
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn a_region_outside_the_frame_is_refused() {
    let frames: Frames = Arc::from(vec![frame("Virtual-1", 8, 8, 0)]);
    let picture =
        Picture::Crop { frames, index: 0, rect: Rect { x: 4, y: 4, width: 8, height: 2 } };
    assert!(save::render(&picture, &[]).is_err());
}

#[test]
fn full_composes_two_outputs_at_the_higher_scale() {
    let frames: Frames = Arc::from(vec![
        frame("Virtual-1", 16, 8, 1),
        frame("Virtual-2", 12, 12, 2),
    ]);
    let placements = [
        Placement { x: 0.0, y: 0.0, scale: 1.0, width: 16, height: 8 },
        Placement { x: 16.0, y: 0.0, scale: 1.5, width: 12, height: 12 },
    ];
    let shot = save::render(&Picture::Compose(frames), &placements).unwrap();
    assert_eq!((shot.width, shot.height), (36, 12));
    let pixel = |shot: &Shot, x: u32, y: u32| {
        let at = ((y * shot.width + x) * 4) as usize;
        [shot.pixels[at], shot.pixels[at + 1], shot.pixels[at + 2]]
    };
    assert_eq!(
        pixel(&shot, 24, 0),
        [2, 0, 0],
        "the scale-1.5 output as it came"
    );
    assert_eq!(pixel(&shot, 35, 11), [2, 11, 11]);
    assert_eq!(
        pixel(&shot, 0, 0)[0],
        1,
        "the scale-1 output, scaled up"
    );
    assert_eq!(pixel(&shot, 23, 11)[0], 1);
}

#[test]
fn saved_files_are_private_and_named_by_time() {
    let temp = Temp::new("modes");
    let dir = temp
        .0
        .join("state/screenshot-notifications");
    let first = save::save(&dir, b"one").unwrap();
    let second = save::save(&dir, b"two").unwrap();
    assert_ne!(first, second);
    for path in [&first, &second] {
        let name = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(save::is_file_name(name), "{name}");
        let mode = fs::metadata(path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
    assert!(
        first.file_name() < second.file_name(),
        "names sort by age"
    );
    assert_eq!(fs::read(&second).unwrap(), b"two");
    let mode = fs::metadata(&dir)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    save::save(&dir, b"three").unwrap();
    let mode = fs::metadata(&dir)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "an existing directory is made private"
    );
}

#[test]
fn the_hundred_newest_stay() {
    let temp = Temp::new("cap");
    let dir = temp.0.join("screenshot-notifications");
    fs::create_dir_all(&dir).unwrap();
    for epoch in 0..105 {
        let name = save::file_name(1_000_000 + epoch, b"AAAAAAAA");
        fs::write(dir.join(name), b"old").unwrap();
    }
    fs::write(dir.join("notes.txt"), b"not a screenshot").unwrap();
    let newest = save::save(&dir, b"new").unwrap();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .unwrap()
        })
        .filter(|name| name.starts_with("screenshot-"))
        .collect();
    names.sort();
    assert_eq!(names.len(), save::KEEP);
    assert_eq!(
        names[0],
        save::file_name(1_000_006, b"AAAAAAAA"),
        "the six oldest went"
    );
    assert!(newest.exists());
    assert!(dir.join("notes.txt").exists(), "other files stay");
}

#[test]
fn a_symlinked_directory_or_parent_is_refused() {
    let temp = Temp::new("symlink");
    let outside = temp.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    let state = temp.0.join("state");
    fs::create_dir_all(&state).unwrap();
    std::os::unix::fs::symlink(&outside, state.join("screenshot-notifications")).unwrap();
    assert!(save::save(&state.join("screenshot-notifications"), b"png").is_err());
    let mode = fs::metadata(&outside)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "the target's mode stays");
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written there"
    );

    let linked_parent = temp.0.join("linked-state");
    std::os::unix::fs::symlink(&outside, &linked_parent).unwrap();
    assert!(
        save::save(
            &linked_parent.join("screenshot-notifications"),
            b"png"
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn pruning_counts_only_managed_regular_files() {
    let temp = Temp::new("strict");
    let dir = temp.0.join("screenshot-notifications");
    fs::create_dir_all(&dir).unwrap();
    for epoch in 0..100 {
        fs::write(
            dir.join(save::file_name(2_000_000 + epoch, b"AAAAAAAA")),
            b"old",
        )
        .unwrap();
    }
    fs::write(
        dir.join("screenshot-0-important.png"),
        b"not ours",
    )
    .unwrap();
    let elsewhere = temp.0.join("elsewhere.png");
    fs::write(&elsewhere, b"outside").unwrap();
    let link = dir.join(save::file_name(1, b"LLLLLLLL"));
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    fs::create_dir(dir.join(save::file_name(2, b"DDDDDDDD"))).unwrap();
    save::save(&dir, b"new").unwrap();
    assert!(
        dir.join("screenshot-0-important.png")
            .exists(),
        "a name not ours stays"
    );
    assert!(
        link.symlink_metadata().is_ok(),
        "a symlink with our name stays"
    );
    assert!(elsewhere.exists());
    assert!(
        dir.join(save::file_name(2, b"DDDDDDDD"))
            .is_dir()
    );
    assert!(
        !dir.join(save::file_name(2_000_000, b"AAAAAAAA"))
            .exists(),
        "the oldest managed file went"
    );
    assert!(
        dir.join(save::file_name(2_000_001, b"AAAAAAAA"))
            .exists()
    );
}

#[test]
fn edit_accepts_only_a_managed_regular_file() {
    let temp = Temp::new("edit");
    let dir = temp.0.join("screenshot-notifications");
    let saved = save::save(&dir, b"png").unwrap();
    let name = saved
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(save::edit_path(&dir, name).unwrap(), saved);

    let elsewhere = temp.0.join("secret.png");
    fs::write(&elsewhere, b"outside").unwrap();
    let swapped = save::file_name(3, b"SSSSSSSS");
    std::os::unix::fs::symlink(&elsewhere, dir.join(&swapped)).unwrap();
    assert!(
        save::edit_path(&dir, &swapped).is_err(),
        "a symlink is refused"
    );
    fs::create_dir(dir.join(save::file_name(4, b"DDDDDDDD"))).unwrap();
    assert!(save::edit_path(&dir, &save::file_name(4, b"DDDDDDDD")).is_err());
    assert!(save::edit_path(&dir, "screenshot-0-important.png").is_err());
    assert!(save::edit_path(&dir, &save::file_name(5, b"MISSINGX")).is_err());
}
