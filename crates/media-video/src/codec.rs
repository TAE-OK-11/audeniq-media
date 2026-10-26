//! Video decoder configurations: what the bitstream's own parameter sets
//! (avcC/hvcC SPS, av1C, vpcC) say about the picture. Only headers are read;
//! no frame is decoded.
use crate::bits::{Bits, unescape};

/// ISO/IEC 23091-2 colour code points (H.264/HEVC VUI, `colr nclx`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub primaries: Option<u16>,
    pub transfer: Option<u16>,
    pub matrix: Option<u16>,
    pub full_range: Option<bool>,
}

impl Color {
    /// Fields this one leaves unset are taken from `other`.
    pub(crate) fn or(self, other: Color) -> Color {
        Color {
            primaries: self.primaries.or(other.primaries),
            transfer: self.transfer.or(other.transfer),
            matrix: self.matrix.or(other.matrix),
            full_range: self.full_range.or(other.full_range),
        }
    }

    /// Code point 2 is "unspecified": no information.
    fn coded(primaries: u32, transfer: u32, matrix: u32, full_range: bool) -> Color {
        let known = |v: u32| (v != 2).then_some(v as u16);
        Color {
            primaries: known(primaries),
            transfer: known(transfer),
            matrix: known(matrix),
            full_range: Some(full_range),
        }
    }
}

/// What a decoder configuration record says about the coded pictures.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    pub profile: Option<String>,
    /// As FFprobe reports it: H.264 `level_idc` (40 = 4.0), HEVC
    /// `general_level_idc` (120 = 4.0), AV1 `seq_level_idx`, VP9 level.
    pub level: Option<u32>,
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma: Option<u32>,
    pub bit_depth: Option<u32>,
    /// Cropped picture size.
    pub size: Option<(u32, u32)>,
    /// `false` when the stream may carry fields (H.264 `frame_mbs_only_flag`
    /// unset, HEVC `field_seq_flag`).
    pub progressive: Option<bool>,
    pub sar: Option<(u32, u32)>,
    pub color: Color,
}

/// H.264 VUI `aspect_ratio_idc` table (Table E-1).
const SAR: [(u32, u32); 17] = [
    (0, 0),
    (1, 1),
    (12, 11),
    (10, 11),
    (16, 11),
    (40, 33),
    (24, 11),
    (20, 11),
    (32, 11),
    (80, 33),
    (18, 11),
    (15, 11),
    (64, 33),
    (160, 99),
    (4, 3),
    (3, 2),
    (2, 1),
];

/// `aspect_ratio_info` … `colour_description` of an H.264/HEVC VUI (the
/// two share this prefix).
fn vui_prefix(b: &mut Bits, cfg: &mut Config) -> Option<()> {
    if b.flag()? {
        let idc = b.u(8)?;
        cfg.sar = match idc {
            255 => Some((b.u(16)?, b.u(16)?)),
            1..=16 => Some(SAR[idc as usize]),
            _ => None,
        }
        .filter(|&(n, d)| n > 0 && d > 0);
    }
    if b.flag()? {
        b.skip(1)?; // overscan_appropriate_flag
    }
    if b.flag()? {
        b.skip(3)?; // video_format
        let full_range = b.flag()?;
        cfg.color = if b.flag()? {
            Color::coded(b.u(8)?, b.u(8)?, b.u(8)?, full_range)
        } else {
            Color {
                full_range: Some(full_range),
                ..Color::default()
            }
        };
    }
    Some(())
}

fn h264_profile(idc: u8, constraints: u8) -> String {
    let set1 = constraints & 0x40 != 0;
    let set3 = constraints & 0x10 != 0;
    match idc {
        66 if set1 => "Constrained Baseline",
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 if set3 => "High 10 Intra",
        110 => "High 10",
        122 if set3 => "High 4:2:2 Intra",
        122 => "High 4:2:2",
        244 if set3 => "High 4:4:4 Intra",
        244 => "High 4:4:4 Predictive",
        44 => "CAVLC 4:4:4",
        _ => return format!("profile {idc}"),
    }
    .to_owned()
}

fn skip_scaling_list(b: &mut Bits, size: u32) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            next = (last + b.se()? + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// H.264 sequence parameter set (ITU-T H.264 7.3.2.1.1), NAL header included.
fn h264_sps(nal: &[u8], cfg: &mut Config) -> Option<()> {
    let rbsp = unescape(nal);
    let mut b = Bits::new(rbsp.get(1..)?);
    let profile_idc = b.u(8)? as u8;
    let constraints = b.u(8)? as u8;
    let level_idc = b.u(8)?;
    b.ue()?; // seq_parameter_set_id
    cfg.profile = Some(h264_profile(profile_idc, constraints));
    cfg.level = Some(level_idc);
    let (mut chroma, mut depth) = (1, 8);
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = b.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            b.skip(1)?; // separate_colour_plane_flag
        }
        depth = b.ue()?.checked_add(8)?;
        b.ue()?; // bit_depth_chroma_minus8
        b.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if b.flag()? {
            let lists = if chroma == 3 { 12 } else { 8 };
            for i in 0..lists {
                if b.flag()? {
                    skip_scaling_list(&mut b, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    cfg.chroma = Some(chroma);
    cfg.bit_depth = Some(depth);
    b.ue()?; // log2_max_frame_num_minus4
    match b.ue()? {
        0 => {
            b.ue()?;
        }
        1 => {
            b.skip(1)?;
            b.se()?;
            b.se()?;
            for _ in 0..b.ue()?.min(256) {
                b.se()?;
            }
        }
        _ => {}
    }
    b.ue()?; // max_num_ref_frames
    b.skip(1)?; // gaps_in_frame_num_value_allowed_flag
    let mbs_w = u64::from(b.ue()?) + 1;
    let map_h = u64::from(b.ue()?) + 1;
    let frame_mbs_only = b.flag()?;
    if !frame_mbs_only {
        b.skip(1)?; // mb_adaptive_frame_field_flag
    }
    b.skip(1)?; // direct_8x8_inference_flag
    let field = if frame_mbs_only { 1 } else { 2 };
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let (crop_x, crop_y) = (sub_w, sub_h * field);
    let (mut w, mut h) = (mbs_w * 16, map_h * 16 * field);
    if b.flag()? {
        let (l, r, t, bo) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        let cx = (u64::from(l) + u64::from(r)) * crop_x;
        let cy = (u64::from(t) + u64::from(bo)) * crop_y;
        w = w.checked_sub(cx)?;
        h = h.checked_sub(cy)?;
    }
    cfg.size = Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?));
    cfg.progressive = Some(frame_mbs_only);
    if b.flag()? {
        vui_prefix(&mut b, cfg)?;
    }
    Some(())
}

/// `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 5.3.3). The profile and
/// level of the record stand when the SPS cannot be read.
pub(crate) fn avcc(rec: &[u8]) -> Option<Config> {
    let mut cfg = Config {
        profile: Some(h264_profile(*rec.get(1)?, *rec.get(2)?)),
        level: Some(u32::from(*rec.get(3)?)),
        ..Config::default()
    };
    if *rec.get(5)? & 0x1f > 0 {
        let len = usize::from(u16::from_be_bytes(rec.get(6..8)?.try_into().ok()?));
        if let Some(sps) = rec.get(8..8 + len) {
            let mut parsed = cfg.clone();
            if h264_sps(sps, &mut parsed).is_some() {
                cfg = parsed;
            }
        }
    }
    Some(cfg)
}

fn hevc_profile(idc: u32, compat: u32) -> String {
    let flag = |i: u32| compat & (1 << (31 - i)) != 0;
    let idc = if idc == 0 {
        (1..32).find(|&i| flag(i)).unwrap_or(0)
    } else {
        idc
    };
    match idc {
        1 => "Main",
        2 => "Main 10",
        3 => "Main Still Picture",
        4 => "Rext",
        9 => "SCC",
        _ => return format!("profile {idc}"),
    }
    .to_owned()
}

/// `profile_tier_level(1, max_sub_layers_minus1)` (H.265 7.3.3).
fn skip_ptl(b: &mut Bits, sub_layers: u32) -> Option<()> {
    b.skip(88)?;
    b.skip(8)?; // general_level_idc
    let mut present = Vec::with_capacity(sub_layers as usize);
    for _ in 0..sub_layers {
        present.push((b.flag()?, b.flag()?));
    }
    if sub_layers > 0 {
        b.skip(2 * (8 - sub_layers as usize))?;
    }
    for (profile, level) in present {
        if profile {
            b.skip(88)?;
        }
        if level {
            b.skip(8)?;
        }
    }
    Some(())
}

fn skip_hevc_scaling_lists(b: &mut Bits) -> Option<()> {
    for size in 0..4u32 {
        let step = if size == 3 { 3 } else { 1 };
        for _ in (0..6).step_by(step) {
            if !b.flag()? {
                b.ue()?;
            } else {
                let coefs = 64.min(1 << (4 + (size << 1)));
                if size > 1 {
                    b.se()?;
                }
                for _ in 0..coefs {
                    b.se()?;
                }
            }
        }
    }
    Some(())
}

/// HEVC sequence parameter set (H.265 7.3.2.2.1), NAL header included.
fn hevc_sps(nal: &[u8], cfg: &mut Config) -> Option<()> {
    let rbsp = unescape(nal);
    let mut b = Bits::new(rbsp.get(2..)?);
    b.skip(4)?; // sps_video_parameter_set_id
    let sub_layers = b.u(3)?;
    if sub_layers > 6 {
        return None;
    }
    b.skip(1)?;
    skip_ptl(&mut b, sub_layers)?;
    b.ue()?; // sps_seq_parameter_set_id
    let chroma = b.ue()?;
    if chroma > 3 {
        return None;
    }
    if chroma == 3 {
        b.skip(1)?;
    }
    let (mut w, mut h) = (u64::from(b.ue()?), u64::from(b.ue()?));
    if b.flag()? {
        let (sub_w, sub_h) = match chroma {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let (l, r, t, bo) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        w = w.checked_sub((u64::from(l) + u64::from(r)) * sub_w)?;
        h = h.checked_sub((u64::from(t) + u64::from(bo)) * sub_h)?;
    }
    cfg.size = Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?));
    cfg.chroma = Some(chroma);
    cfg.bit_depth = Some(b.ue()?.checked_add(8)?);
    b.ue()?; // bit_depth_chroma_minus8
    let poc_lsb_bits = b.ue()?.checked_add(4)?;
    if poc_lsb_bits > 16 {
        return None;
    }
    let all = b.flag()?;
    for _ in (if all { 0 } else { sub_layers })..=sub_layers {
        b.ue()?;
        b.ue()?;
        b.ue()?;
    }
    for _ in 0..6 {
        b.ue()?; // coding/transform block sizes and hierarchy depths
    }
    if b.flag()? && b.flag()? {
        skip_hevc_scaling_lists(&mut b)?;
    }
    b.skip(2)?; // amp_enabled_flag, sample_adaptive_offset_enabled_flag
    if b.flag()? {
        b.skip(8)?;
        b.ue()?;
        b.ue()?;
        b.skip(1)?;
    }
    let sets = b.ue()?;
    if sets > 64 {
        return None;
    }
    let mut deltas: Vec<u32> = Vec::with_capacity(sets as usize);
    for idx in 0..sets {
        let predicted = idx != 0 && b.flag()?;
        if predicted {
            b.skip(1)?; // delta_rps_sign
            b.ue()?; // abs_delta_rps_minus1
            let reference = *deltas.last()?;
            let mut n = 0;
            for _ in 0..=reference {
                let used = b.flag()?;
                if used || b.flag()? {
                    n += 1;
                }
            }
            deltas.push(n);
        } else {
            let (neg, pos) = (b.ue()?, b.ue()?);
            if neg > 16 || pos > 16 {
                return None;
            }
            for _ in 0..neg + pos {
                b.ue()?;
                b.skip(1)?;
            }
            deltas.push(neg + pos);
        }
    }
    if b.flag()? {
        let n = b.ue()?;
        if n > 32 {
            return None;
        }
        for _ in 0..n {
            b.skip(poc_lsb_bits as usize + 1)?;
        }
    }
    b.skip(2)?; // temporal_mvp, strong_intra_smoothing
    cfg.progressive = Some(true);
    if b.flag()? {
        vui_prefix(&mut b, cfg)?;
        if b.flag()? {
            b.ue()?;
            b.ue()?;
        }
        b.skip(1)?; // neutral_chroma_indication_flag
        if b.flag()? {
            cfg.progressive = Some(false); // field_seq_flag
        }
    }
    Some(())
}

/// `HEVCDecoderConfigurationRecord` (ISO/IEC 14496-15 8.3.3).
pub(crate) fn hvcc(rec: &[u8]) -> Option<Config> {
    let ptl = *rec.get(1)?;
    let compat = u32::from_be_bytes(rec.get(2..6)?.try_into().ok()?);
    let mut cfg = Config {
        profile: Some(hevc_profile(u32::from(ptl & 0x1f), compat)),
        level: Some(u32::from(*rec.get(12)?)),
        chroma: Some(u32::from(*rec.get(16)? & 3)),
        bit_depth: Some(u32::from(*rec.get(17)? & 7) + 8),
        ..Config::default()
    };
    let arrays = *rec.get(22)?;
    let mut at = 23;
    for _ in 0..arrays {
        let kind = *rec.get(at)? & 0x3f;
        let n = u16::from_be_bytes(rec.get(at + 1..at + 3)?.try_into().ok()?);
        at += 3;
        for _ in 0..n {
            let len = usize::from(u16::from_be_bytes(rec.get(at..at + 2)?.try_into().ok()?));
            let nal = rec.get(at + 2..at + 2 + len)?;
            at += 2 + len;
            if kind == 33 {
                let mut parsed = cfg.clone();
                if hevc_sps(nal, &mut parsed).is_some() {
                    cfg = parsed;
                }
                return Some(cfg);
            }
        }
    }
    Some(cfg)
}

/// `AV1CodecConfigurationRecord` (AV1 ISOBMFF binding 2.3).
pub(crate) fn av1c(rec: &[u8]) -> Option<Config> {
    let (b1, b2) = (*rec.get(1)?, *rec.get(2)?);
    let profile = b1 >> 5;
    let depth = match (b2 & 0x40 != 0, b2 & 0x20 != 0) {
        (false, _) => 8,
        (true, false) => 10,
        (true, true) => 12,
    };
    let chroma = match (b2 & 0x10 != 0, b2 & 0x08 != 0, b2 & 0x04 != 0) {
        (true, _, _) => 0,
        (_, true, true) => 1,
        (_, true, false) => 2,
        _ => 3,
    };
    Some(Config {
        profile: Some(
            match profile {
                0 => "Main",
                1 => "High",
                2 => "Professional",
                _ => "unknown",
            }
            .to_owned(),
        ),
        level: Some(u32::from(b1 & 0x1f)),
        chroma: Some(chroma),
        bit_depth: Some(depth),
        ..Config::default()
    })
}

/// `VPCodecConfigurationRecord` (VP9 ISOBMFF binding), a full box.
pub(crate) fn vpcc(rec: &[u8]) -> Option<Config> {
    let profile = *rec.get(4)?;
    let packed = *rec.get(6)?;
    let chroma = match packed >> 1 & 7 {
        0 | 1 => 1,
        2 => 2,
        3 => 3,
        _ => return None,
    };
    let mut cfg = Config {
        profile: Some(format!("Profile {profile}")),
        level: Some(u32::from(*rec.get(5)?)),
        chroma: Some(chroma),
        bit_depth: Some(u32::from(packed >> 4)),
        ..Config::default()
    };
    if let Some(c) = rec.get(7..10) {
        cfg.color = Color::coded(
            u32::from(c[0]),
            u32::from(c[1]),
            u32::from(c[2]),
            packed & 1 == 1,
        );
    }
    Some(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// x264 1920×1080 High@4.0 SPS (1088 coded rows, cropped), with VUI.
    const SPS_1080: [u8; 29] = [
        0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x5a, 0x80, 0x80,
        0x80, 0xa0, 0x00, 0x00, 0x7d, 0x20, 0x00, 0x1d, 0x4c, 0x01, 0xe3, 0x06, 0x32, 0xc0,
    ];

    #[test]
    fn h264_sps_crops_1088_to_1080() {
        let mut cfg = Config::default();
        assert!(h264_sps(&SPS_1080, &mut cfg).is_some());
        assert_eq!(cfg.profile.as_deref(), Some("High"));
        assert_eq!(cfg.level, Some(40));
        assert_eq!(cfg.size, Some((1920, 1080)));
        assert_eq!((cfg.chroma, cfg.bit_depth), (Some(1), Some(8)));
        assert_eq!(cfg.progressive, Some(true));
        assert_eq!(cfg.sar, Some((1, 1)));
    }

    #[test]
    fn record_profile_survives_an_unreadable_sps() {
        let rec = [1, 77, 0x40, 31, 0xff, 0xe1, 0, 2, 0x67, 77];
        let cfg = avcc(&rec).unwrap();
        assert_eq!(cfg.profile.as_deref(), Some("Main"));
        assert_eq!(cfg.level, Some(31));
        assert_eq!(cfg.size, None);
    }

    #[test]
    fn short_records_are_rejected() {
        assert!(avcc(&[1, 100]).is_none());
        assert!(hvcc(&[1; 10]).is_none());
        assert!(av1c(&[0x81]).is_none());
        assert!(vpcc(&[0; 5]).is_none());
    }
}
