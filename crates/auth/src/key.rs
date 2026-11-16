//! YubiKey presence from the hidraw devices in sysfs.

use std::path::Path;

pub const YUBICO_VENDOR: u32 = 0x1050;
pub const HIDRAW_CLASS: &str = "/sys/class/hidraw";

pub fn uevent_vendor(uevent: &str) -> Option<u32> {
    let id = uevent
        .lines()
        .find_map(|line| line.strip_prefix("HID_ID="))?;
    let mut parts = id.trim().split(':');
    let (bus, vendor, product) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    u32::from_str_radix(bus, 16).ok()?;
    u32::from_str_radix(product, 16).ok()?;
    u32::from_str_radix(vendor, 16).ok()
}

pub fn key_present(hidraw_class: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(hidraw_class) else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read_to_string(entry.path().join("device/uevent"))
            .is_ok_and(|uevent| uevent_vendor(&uevent) == Some(YUBICO_VENDOR))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::tests_support::TempDir;

    const YUBIKEY: &str = "DRIVER=hid-generic\nHID_ID=0003:00001050:00000407\n\
                           HID_NAME=Yubico YubiKey OTP+FIDO+CCID\nMODALIAS=hid:b0003g0001v00001050p00000407\n";
    const MOUSE: &str = "DRIVER=hid-generic\nHID_ID=0003:0000046D:0000C52B\nHID_NAME=Logitech\n";

    fn device(class: &Path, name: &str, uevent: Option<&str>) {
        let dir = class.join(name).join("device");
        fs::create_dir_all(&dir).unwrap();
        if let Some(uevent) = uevent {
            fs::write(dir.join("uevent"), uevent).unwrap();
        }
    }

    #[test]
    fn parses_hid_id() {
        assert_eq!(uevent_vendor(YUBIKEY), Some(0x1050));
        assert_eq!(uevent_vendor(MOUSE), Some(0x046d));
        assert_eq!(
            uevent_vendor("HID_ID=0005:00001050:00000407"),
            Some(0x1050)
        );
        assert_eq!(uevent_vendor("HID_NAME=x\n"), None);
        assert_eq!(uevent_vendor("HID_ID=0003:00001050"), None);
        assert_eq!(uevent_vendor("HID_ID=0003:zz:00000407"), None);
        assert_eq!(
            uevent_vendor("HID_ID=0003:00001050:00000407:1"),
            None
        );
        assert_eq!(
            uevent_vendor("MODALIAS=hid:b0003g0001v00001050p00000407\n"),
            None
        );
        assert_eq!(
            uevent_vendor("XHID_ID=0003:00001050:00000407"),
            None
        );
    }

    #[test]
    fn product_id_is_not_vendor() {
        assert_eq!(
            uevent_vendor("HID_ID=0003:0000046D:00001050\n"),
            Some(0x046d)
        );
    }

    #[test]
    fn fake_sysfs() {
        let root = TempDir::new("hidraw");
        let class = root.path().join("hidraw");
        assert!(!key_present(&class));
        fs::create_dir_all(&class).unwrap();
        assert!(!key_present(&class));
        device(&class, "hidraw0", Some(MOUSE));
        device(&class, "hidraw1", None);
        assert!(!key_present(&class));
        device(&class, "hidraw2", Some(YUBIKEY));
        assert!(key_present(&class));
        fs::remove_dir_all(class.join("hidraw2")).unwrap();
        assert!(!key_present(&class));
    }

    #[test]
    fn unreadable_device_is_skipped() {
        let root = TempDir::new("hidraw-bad");
        let class = root.path().join("hidraw");
        fs::create_dir_all(class.join("hidraw0/device/uevent")).unwrap();
        fs::write(class.join("stray-file"), "x").unwrap();
        assert!(!key_present(&class));
        device(&class, "hidraw1", Some(YUBIKEY));
        assert!(key_present(&class));
    }
}
