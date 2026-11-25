pub mod h264_mft;
pub mod video_processor;

use std::sync::Arc;

/// Paquete de video codificado (H.264) en formato Annex-B listo para el Ring Buffer.
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

/// Detecta si un buffer H.264 Annex-B contiene un Keyframe (IDR, NAL tipo 5)
/// sin alocar memoria y con corte temprano al encontrar la primera slice no-keyframe.
pub fn annexb_contains_idr(data: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some((_, nal_start)) = find_next_start_code(data, pos) {
        if nal_start >= data.len() {
            break;
        }
        match data[nal_start] & 0x1F {
            5 => return true,
            1 => return false,
            _ => pos = nal_start + 1,
        }
    }
    false
}

/// Detecta si un buffer H.264 Annex-B ya incluye cabeceras de secuencia (SPS, NAL tipo 7).
pub fn annexb_contains_headers(data: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some((_, nal_start)) = find_next_start_code(data, pos) {
        if nal_start >= data.len() {
            break;
        }
        match data[nal_start] & 0x1F {
            7 => return true,
            1 | 5 => return false,
            _ => pos = nal_start + 1,
        }
    }
    false
}

/// Extrae las cabeceras de parámetro H.264 SPS (7) + PPS (8) con sus start codes Annex-B originales.
pub fn extract_sps_pps_annexb(data: &[u8]) -> Option<Vec<u8>> {
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
        assert!(annexb_contains_headers(&idr));
        assert!(annexb_contains_idr(&idr));
        let headers = extract_sps_pps_annexb(&idr).expect("SPS+PPS");
        assert_eq!(headers.len(), 16);

        let p_frame = [0, 0, 0, 1, 0x41, 0x9A, 0x01, 0x00, 0x00, 0x01, 0x65];
        assert!(!annexb_contains_headers(&p_frame));
        assert!(!annexb_contains_idr(&p_frame));
    }
}
