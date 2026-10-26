//! ISO base media / QuickTime box helpers over an in-memory payload.

pub(crate) fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        b.get(i..i.checked_add(2)?)?.try_into().ok()?,
    ))
}
pub(crate) fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(i..i.checked_add(4)?)?.try_into().ok()?,
    ))
}
pub(crate) fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        b.get(i..i.checked_add(8)?)?.try_into().ok()?,
    ))
}
pub(crate) fn fourcc(b: &[u8]) -> [u8; 4] {
    let mut f = [b' '; 4];
    for (d, s) in f.iter_mut().zip(b) {
        *d = *s;
    }
    f
}

/// Printable four-character code (others as `?`).
pub(crate) fn fourcc_str(f: &[u8; 4]) -> String {
    f.iter()
        .map(|&c| {
            if (0x20..0x7f).contains(&c) {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Child boxes of a payload as (type, payload). Stops at the first box that
/// does not fit; a size of 0 runs to the end.
pub(crate) fn children(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let size = u64::from(u32_at(b, 0)?);
        let typ = fourcc(b.get(4..8)?);
        let (hdr, size) = match size {
            0 => (8, b.len() as u64),
            1 => (16, u64_at(b, 8)?),
            n => (8, n),
        };
        let size = usize::try_from(size).ok()?;
        if size < hdr || size > b.len() {
            return None;
        }
        let payload = &b[hdr..size];
        b = &b[size..];
        Some((typ, payload))
    })
}

pub(crate) fn child<'a>(b: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    children(b).find(|(t, _)| t == name).map(|(_, p)| p)
}

/// `a/b/c` below `b`.
pub(crate) fn path<'a>(mut b: &'a [u8], names: &[&[u8; 4]]) -> Option<&'a [u8]> {
    for n in names {
        b = child(b, n)?;
    }
    Some(b)
}
