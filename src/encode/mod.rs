pub mod h264_mft;
pub mod video_processor;

/// Paquete H.264 codificado en formato Annex-B listo para entrar al Ring Buffer en RAM.
#[derive(Debug, Clone)]
pub struct EncodedVideoPacket {
    /// Bitstream H.264 (NAL units con start codes Annex-B).
    pub data: Vec<u8>,
    /// Timestamp de presentación en ticks de 100 ns.
    pub pts_100ns: i64,
    /// Duración nominal del frame en ticks de 100 ns.
    pub duration_100ns: i64,
    /// `true` si contiene un Keyframe (IDR) desde el cual se puede iniciar un clip limpio.
    pub is_keyframe: bool,
}

/// Itera sobre las NAL units en un buffer H.264 Annex-B devolviendo `(start_code_start, nal_header_offset, nal_end)`.
fn iter_annexb_nals(data: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, i + 3));
                i += 3;
                continue;
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }

    let mut result = Vec::with_capacity(starts.len());
    for idx in 0..starts.len() {
        let (sc_start, nal_start) = starts[idx];
        let nal_end = if idx + 1 < starts.len() {
            starts[idx + 1].0
        } else {
            data.len()
        };
        if nal_start < nal_end {
            result.push((sc_start, nal_start, nal_end));
        }
    }
    result
}

/// Detecta si un buffer H.264 Annex-B contiene una NAL unit IDR (tipo 5).
pub fn annexb_contains_idr(data: &[u8]) -> bool {
    iter_annexb_nals(data)
        .into_iter()
        .any(|(_, nal_start, _)| (data[nal_start] & 0x1F) == 5)
}

/// Detecta si un buffer H.264 Annex-B ya incluye una NAL unit SPS (tipo 7).
pub fn annexb_contains_sps(data: &[u8]) -> bool {
    iter_annexb_nals(data)
        .into_iter()
        .any(|(_, nal_start, _)| (data[nal_start] & 0x1F) == 7)
}

/// Extrae las NAL units SPS (tipo 7) y PPS (tipo 8) con sus start codes Annex-B originales.
/// Devuelve `Some(bytes)` solo si ambas cabeceras (SPS y PPS) están presentes.
pub fn extract_sps_pps_annexb(data: &[u8]) -> Option<Vec<u8>> {
    let mut sps: Option<&[u8]> = None;
    let mut pps: Option<&[u8]> = None;

    for (sc_start, nal_start, nal_end) in iter_annexb_nals(data) {
        let nal_type = data[nal_start] & 0x1F;
        match nal_type {
            7 if sps.is_none() => sps = Some(&data[sc_start..nal_end]),
            8 if pps.is_none() => pps = Some(&data[sc_start..nal_end]),
            _ => {}
        }
    }

    if let (Some(sps_bytes), Some(pps_bytes)) = (sps, pps) {
        let mut out = Vec::with_capacity(sps_bytes.len() + pps_bytes.len());
        out.extend_from_slice(sps_bytes);
        out.extend_from_slice(pps_bytes);
        Some(out)
    } else {
        None
    }
}
