pub mod h264_mft;
pub mod video_processor;

use std::sync::Arc;

use crate::config::VideoCodec;

/// Paquete de video codificado (H.264 o HEVC/H.265) en formato Annex-B listo para el Ring Buffer.
///
/// Usa `Arc<[u8]>` para que `ReplayBuffer::snapshot()` sea **Zero-Copy** (~0.05 ms y 0 MB extra
/// al guardar clips de 30–120 s sin congelar el hilo de grabación).
#[derive(Debug, Clone)]
pub struct EncodedVideoPacket {
    /// Bitstream Annex-B (NAL units con start codes `00 00 01` o `00 00 00 01`).
    pub data: Arc<[u8]>,
    /// Timestamp de presentación en ticks de 100 ns.
    pub pts_100ns: i64,
    /// Duración nominal del frame en ticks de 100 ns.
    pub duration_100ns: i64,
    /// `true` si contiene un Keyframe (IDR / IRAP) desde el cual se puede iniciar un clip limpio.
    pub is_keyframe: bool,
}

/// Busca el próximo start code Annex-B (`00 00 01` o `00 00 00 01`) a partir de `from`.
/// Devuelve `(start_code_offset, nal_header_offset)`.
#[inline]
fn find_next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let len = data.len();
    let mut i = from;
    while i + 3 <= len {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                return Some((i, i + 3));
            }
            if i + 4 <= len && data[i + 2] == 0 && data[i + 3] == 1 {
                return Some((i, i + 4));
            }
        }
        i += 1;
    }
    None
}

/// Iterador zero-allocation sobre las NAL units de un buffer Annex-B.
/// Produce `(start_code_start, nal_header_offset, nal_end)`.
struct AnnexBNalIter<'a> {
    data: &'a [u8],
    next_start: Option<(usize, usize)>,
}

impl<'a> AnnexBNalIter<'a> {
    #[inline]
    fn new(data: &'a [u8]) -> Self {
        let next_start = find_next_start_code(data, 0);
        Self { data, next_start }
    }
}

impl<'a> Iterator for AnnexBNalIter<'a> {
    type Item = (usize, usize, usize);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (sc_start, nal_start) = self.next_start?;
            let following = find_next_start_code(self.data, nal_start);
            let nal_end = following.map_or(self.data.len(), |(next_sc, _)| next_sc);
            self.next_start = following;
            if nal_start < nal_end {
                return Some((sc_start, nal_start, nal_end));
            }
        }
    }
}

/// Detecta si un buffer Annex-B contiene un Keyframe (IDR en H.264 o IRAP/IDR/CRA en HEVC)
/// sin alocar memoria y con corte temprano al encontrar la primera slice no-keyframe.
pub fn annexb_contains_idr_for(data: &[u8], codec: VideoCodec) -> bool {
    let mut pos = 0usize;
    while let Some((_, nal_start)) = find_next_start_code(data, pos) {
        if nal_start >= data.len() {
            break;
        }
        match codec {
            VideoCodec::H264 => match data[nal_start] & 0x1F {
                5 => return true,
                1 => return false,
                _ => pos = nal_start + 1,
            },
            VideoCodec::Hevc => match (data[nal_start] >> 1) & 0x3F {
                16..=21 => return true, // BLA / IDR_W_RADL / IDR_N_LP / CRA
                0..=9 => return false,  // TRAIL / TSA / STSA / RADL / RASL (no-keyframe)
                _ => pos = nal_start + 1,
            },
        }
    }
    false
}

#[allow(dead_code)]
pub fn annexb_contains_idr(data: &[u8]) -> bool {
    annexb_contains_idr_for(data, VideoCodec::H264)
}

/// Detecta si un buffer Annex-B ya incluye cabeceras de secuencia (SPS en H.264, VPS/SPS en HEVC).
pub fn annexb_contains_headers_for(data: &[u8], codec: VideoCodec) -> bool {
    let mut pos = 0usize;
    while let Some((_, nal_start)) = find_next_start_code(data, pos) {
        if nal_start >= data.len() {
            break;
        }
        match codec {
            VideoCodec::H264 => match data[nal_start] & 0x1F {
                7 => return true,
                1 | 5 => return false,
                _ => pos = nal_start + 1,
            },
            VideoCodec::Hevc => match (data[nal_start] >> 1) & 0x3F {
                32 | 33 => return true, // VPS o SPS
                0..=21 => return false,
                _ => pos = nal_start + 1,
            },
        }
    }
    false
}

/// Extrae las cabeceras de parámetro con sus start codes Annex-B originales:
/// - H.264: SPS (7) + PPS (8)
/// - HEVC: VPS (32) + SPS (33) + PPS (34)
pub fn extract_codec_headers_annexb(data: &[u8], codec: VideoCodec) -> Option<Vec<u8>> {
    match codec {
        VideoCodec::H264 => {
            let mut sps: Option<&[u8]> = None;
            let mut pps: Option<&[u8]> = None;
            for (sc_start, nal_start, nal_end) in AnnexBNalIter::new(data) {
                match data[nal_start] & 0x1F {
                    7 if sps.is_none() => sps = Some(&data[sc_start..nal_end]),
                    8 if pps.is_none() => pps = Some(&data[sc_start..nal_end]),
                    1 | 5 if sps.is_some() && pps.is_some() => break,
                    _ => {}
                }
                if sps.is_some() && pps.is_some() {
                    break;
                }
            }
            if let (Some(sps_b), Some(pps_b)) = (sps, pps) {
                let mut out = Vec::with_capacity(sps_b.len() + pps_b.len());
                out.extend_from_slice(sps_b);
                out.extend_from_slice(pps_b);
                Some(out)
            } else {
                None
            }
        }
        VideoCodec::Hevc => {
            let mut vps: Option<&[u8]> = None;
            let mut sps: Option<&[u8]> = None;
            let mut pps: Option<&[u8]> = None;
            for (sc_start, nal_start, nal_end) in AnnexBNalIter::new(data) {
                match (data[nal_start] >> 1) & 0x3F {
                    32 if vps.is_none() => vps = Some(&data[sc_start..nal_end]),
                    33 if sps.is_none() => sps = Some(&data[sc_start..nal_end]),
                    34 if pps.is_none() => pps = Some(&data[sc_start..nal_end]),
                    0..=21 if vps.is_some() && sps.is_some() && pps.is_some() => break,
                    _ => {}
                }
                if vps.is_some() && sps.is_some() && pps.is_some() {
                    break;
                }
            }
            if let (Some(sps_b), Some(pps_b)) = (sps, pps) {
                let vps_len = vps.map_or(0, |v| v.len());
                let mut out = Vec::with_capacity(vps_len + sps_b.len() + pps_b.len());
                if let Some(vps_b) = vps {
                    out.extend_from_slice(vps_b);
                }
                out.extend_from_slice(sps_b);
                out.extend_from_slice(pps_b);
                Some(out)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_annexb_nals_zero_alloc() {
        let idr = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, // SPS (7)
            0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS (8)
            0, 0, 1, 0x65, 0x88, 0x84, // IDR (5)
        ];
        assert!(annexb_contains_headers_for(&idr, VideoCodec::H264));
        assert!(annexb_contains_idr_for(&idr, VideoCodec::H264));
        let headers = extract_codec_headers_annexb(&idr, VideoCodec::H264).expect("SPS+PPS");
        assert_eq!(headers.len(), 16);

        let p_frame = [0, 0, 0, 1, 0x41, 0x9A, 0x01, 0x00, 0x00, 0x01, 0x65];
        assert!(!annexb_contains_headers_for(&p_frame, VideoCodec::H264));
        assert!(!annexb_contains_idr_for(&p_frame, VideoCodec::H264));

        // HEVC: VPS (32 -> 0x40), SPS (33 -> 0x42), PPS (34 -> 0x44), IDR_W_RADL (19 -> 0x26)
        let hevc_idr = [
            0, 0, 0, 1, 0x40, 0x01, 0xAA,
            0, 0, 0, 1, 0x42, 0x01, 0xBB,
            0, 0, 0, 1, 0x44, 0x01, 0xCC,
            0, 0, 0, 1, 0x26, 0x01, 0xDD,
        ];
        assert!(annexb_contains_headers_for(&hevc_idr, VideoCodec::Hevc));
        assert!(annexb_contains_idr_for(&hevc_idr, VideoCodec::Hevc));
        let hevc_hdr = extract_codec_headers_annexb(&hevc_idr, VideoCodec::Hevc).expect("VPS+SPS+PPS");
        assert_eq!(hevc_hdr.len(), 21);

        let hevc_p = [0, 0, 0, 1, 0x02, 0x01, 0x99]; // TRAIL_R (1 -> 0x02)
        assert!(!annexb_contains_idr_for(&hevc_p, VideoCodec::Hevc));
    }
}
