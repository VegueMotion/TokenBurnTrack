use tbt_core::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed(u64),
}

pub(crate) struct Fields<'a> {
    buf: &'a [u8],
}

pub(crate) fn fields(buf: &[u8]) -> Fields<'_> {
    Fields { buf }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Wire<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.parse();
        if item.is_none() {
            self.buf = &[];
        }
        item
    }
}

impl<'a> Fields<'a> {
    fn parse(&mut self) -> Option<(u64, Wire<'a>)> {
        if self.buf.is_empty() {
            return None;
        }
        let key = self.varint()?;
        let wire = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::Fixed(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            2 => {
                let len = usize::try_from(self.varint()?).ok()?;
                Wire::Bytes(self.take(len)?)
            }
            5 => Wire::Fixed(u64::from(u32::from_le_bytes(
                self.take(4)?.try_into().ok()?,
            ))),
            _ => return None,
        };
        Some((key >> 3, wire))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for (i, &b) in self.buf.iter().enumerate().take(10) {
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                self.buf = &self.buf[i + 1..];
                return Some(value);
            }
        }
        None
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.buf.len() < len {
            return None;
        }
        let (head, rest) = self.buf.split_at(len);
        self.buf = rest;
        Some(head)
    }
}

pub(crate) fn varint(buf: &[u8], field: u64) -> Option<u64> {
    fields(buf)
        .filter_map(|(n, w)| match w {
            Wire::Varint(v) if n == field => Some(v),
            _ => None,
        })
        .last()
}

pub(crate) fn bytes(buf: &[u8], field: u64) -> Option<&[u8]> {
    fields(buf)
        .filter_map(|(n, w)| match w {
            Wire::Bytes(b) if n == field => Some(b),
            _ => None,
        })
        .last()
}

pub(crate) fn string(buf: &[u8], field: u64) -> Option<&str> {
    bytes(buf, field).and_then(|b| std::str::from_utf8(b).ok())
}

pub(crate) fn timestamp(buf: &[u8]) -> Option<Timestamp> {
    let seconds = i64::try_from(varint(buf, 1)?).ok().filter(|s| *s > 0)?;
    let nanos = varint(buf, 2)
        .and_then(|n| i32::try_from(n).ok())
        .filter(|n| (0..1_000_000_000).contains(n))
        .unwrap_or(0);
    Timestamp::new(seconds, nanos).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_fields_and_stops_at_garbage() {
        let inner = [0x08, 0x96, 0x01, 0x5a, 0x02, b'h', b'i'];
        let mut buf = vec![0x0a, inner.len() as u8];
        buf.extend_from_slice(&inner);
        buf.extend_from_slice(&[0x15, 1, 0, 0, 0, 0x18, 7, 0x18, 9]);
        let nested = bytes(&buf, 1).unwrap();
        assert_eq!(varint(nested, 1), Some(150));
        assert_eq!(string(nested, 11), Some("hi"));
        assert_eq!(varint(&buf, 3), Some(9));
        assert_eq!(fields(&buf).count(), 4);

        assert_eq!(varint(&[0x08, 0x96], 1), None);
        assert_eq!(fields(&[0x0a, 0x05, 1]).count(), 0);
        assert_eq!(fields(&[0x0f, 0x00]).count(), 0);
    }

    #[test]
    fn timestamps_need_positive_seconds() {
        let ts = timestamp(&[0x08, 0xc0, 0x8d, 0xf6, 0xd7, 0x06, 0x10, 0x05]).unwrap();
        assert_eq!(ts.as_second(), 1_795_000_000);
        assert_eq!(ts.subsec_nanosecond(), 5);
        assert_eq!(timestamp(&[0x10, 0x05]), None);
    }
}
