//! A tagstruct reader for info replies that never reads or allocates past the received frame.

use pulseaudio::protocol::{ChannelVolume, Props, Tag, Volume};

pub(crate) const KEPT: &[&str] = &[
    "application.name",
    "application.process.id",
    "client.api",
    "client.id",
    "device.api",
    "node.group",
    "node.link-group",
    "node.name",
    "object.id",
    "pipewire.sec.pid",
    "stream.monitor",
];

// A longer value is treated as absent, so a cut group name can never match another.
pub(crate) const MAX_VALUE: usize = 1024;

#[derive(Debug, thiserror::Error)]
#[error("bad tagstruct at byte {at}: {what}")]
pub(crate) struct WireError {
    pub at: usize,
    pub what: &'static str,
}

// A tagstruct has no entry lengths, so a bad entry ends the read. Earlier entries come back.
pub(crate) fn read_entries<T>(
    body: &[u8],
    mut read: impl FnMut(&mut Wire<'_>) -> Result<T, WireError>,
) -> (Vec<T>, Option<WireError>) {
    let mut wire = Wire::new(body);
    let mut entries = Vec::new();
    while !wire.at_end() {
        match read(&mut wire) {
            Ok(entry) => entries.push(entry),
            Err(error) => return (entries, Some(error)),
        }
    }
    (entries, None)
}

pub(crate) struct Wire<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Wire<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn fail<T>(&self, what: &'static str) -> Result<T, WireError> {
        Err(WireError { at: self.pos, what })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        match self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.buf.len())
        {
            Some(end) => {
                let bytes = &self.buf[self.pos..end];
                self.pos = end;
                Ok(bytes)
            }
            None => self.fail("truncated"),
        }
    }

    fn byte(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn be32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn tag(&mut self, tag: Tag) -> Result<(), WireError> {
        if self.byte()? == tag as u8 {
            Ok(())
        } else {
            self.pos -= 1;
            self.fail("unexpected tag")
        }
    }

    pub fn u32(&mut self) -> Result<u32, WireError> {
        self.tag(Tag::U32)?;
        self.be32()
    }

    pub fn index(&mut self) -> Result<Option<u32>, WireError> {
        Ok(Some(self.u32()?).filter(|&i| i != u32::MAX))
    }

    pub fn u8(&mut self) -> Result<u8, WireError> {
        self.tag(Tag::U8)?;
        self.byte()
    }

    pub fn bool(&mut self) -> Result<bool, WireError> {
        match self.byte()? {
            b if b == Tag::BooleanTrue as u8 => Ok(true),
            b if b == Tag::BooleanFalse as u8 => Ok(false),
            _ => {
                self.pos -= 1;
                self.fail("expected a boolean")
            }
        }
    }

    pub fn usec(&mut self) -> Result<(), WireError> {
        self.tag(Tag::Usec)?;
        self.take(8).map(drop)
    }

    pub fn string(&mut self) -> Result<Option<&'a [u8]>, WireError> {
        match self.byte()? {
            b if b == Tag::StringNull as u8 => Ok(None),
            b if b == Tag::String as u8 => {
                let rest = &self.buf[self.pos..];
                let Some(len) = rest.iter().position(|&b| b == 0) else {
                    return self.fail("unterminated string");
                };
                self.pos += len + 1;
                Ok(Some(&rest[..len]))
            }
            _ => {
                self.pos -= 1;
                self.fail("expected a string")
            }
        }
    }

    pub fn text(&mut self) -> Result<String, WireError> {
        Ok(self
            .string()?
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default())
    }

    pub fn sample_spec(&mut self) -> Result<(), WireError> {
        self.tag(Tag::SampleSpec)?;
        self.take(6).map(drop)
    }

    pub fn channel_map(&mut self) -> Result<(), WireError> {
        self.tag(Tag::ChannelMap)?;
        let n = self.byte()?;
        self.take(usize::from(n)).map(drop)
    }

    pub fn cvolume(&mut self) -> Result<ChannelVolume, WireError> {
        self.tag(Tag::CVolume)?;
        let n = self.byte()?;
        let mut volume = ChannelVolume::empty();
        for _ in 0..n {
            volume.push(Volume::from_u32_clamped(self.be32()?));
        }
        Ok(volume)
    }

    pub fn volume(&mut self) -> Result<(), WireError> {
        self.tag(Tag::Volume)?;
        self.take(4).map(drop)
    }

    pub fn format(&mut self) -> Result<(), WireError> {
        self.tag(Tag::FormatInfo)?;
        self.u8()?;
        self.props().map(drop)
    }

    pub fn props(&mut self) -> Result<Props, WireError> {
        self.tag(Tag::PropList)?;
        let mut props = Props::new();
        while let Some(key) = self.string()? {
            let len = self.u32()? as usize;
            self.tag(Tag::Arbitrary)?;
            if self.be32()? as usize != len {
                return self.fail("proplist value length mismatch");
            }
            let value = self.take(len)?;
            if len <= MAX_VALUE
                && KEPT.iter().any(|k| k.as_bytes() == key)
                && let Ok(key) = std::ffi::CString::new(key)
            {
                props.set_bytes(key, value);
            }
        }
        Ok(props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulseaudio::protocol::{MAX_VERSION, TagStructWriter};

    fn proplist(pairs: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut ts = TagStructWriter::new(&mut out, MAX_VERSION);
        let mut props = Props::new();
        for (key, value) in pairs {
            props.set_bytes(std::ffi::CString::new(*key).unwrap(), value);
        }
        ts.write(&props).unwrap();
        out
    }

    #[test]
    fn keeps_only_known_keys_that_fit() {
        let huge = vec![b'x'; 200_000];
        let long_group = vec![b'g'; MAX_VALUE + 1];
        let body = proplist(&[
            ("application.name", b"App\0"),
            ("media.software", &huge),
            ("node.group", &long_group),
            ("node.link-group", b"lg\0"),
        ]);
        let mut wire = Wire::new(&body);
        let props = wire.props().expect("read");
        assert!(wire.at_end());
        assert_eq!(
            props.get_bytes(c"application.name"),
            Some(&b"App\0"[..])
        );
        assert_eq!(
            props.get_bytes(c"node.link-group"),
            Some(&b"lg\0"[..])
        );
        assert_eq!(props.get_bytes(c"node.group"), None);
        assert_eq!(props.get_bytes(c"media.software"), None);
    }

    #[test]
    fn a_length_past_the_frame_fails_without_allocating() {
        let mut body = vec![Tag::PropList as u8, Tag::String as u8, b'k', 0];
        body.push(Tag::U32 as u8);
        body.extend_from_slice(&u32::MAX.to_be_bytes());
        body.push(Tag::Arbitrary as u8);
        body.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(Wire::new(&body).props().is_err());
    }

    #[test]
    fn a_truncated_frame_fails() {
        let body = proplist(&[("application.name", b"App\0")]);
        for cut in 0..body.len() {
            assert!(
                Wire::new(&body[..cut]).props().is_err(),
                "cut at {cut}"
            );
        }
    }
}
