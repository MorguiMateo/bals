use std::collections::VecDeque;

use crate::audio::{AudioStreamInfo, EncodedAudioPacket};
use crate::encode::{EncodedVideoPacket, extract_sps_pps_annexb};

/// Instantánea inmutable y **Zero-Copy** (`Arc<[u8]>`) de los últimos N segundos de video H.264
/// + audio AAC comprimidos, lista para muxing MP4 en un hilo secundario sin bloquear la grabación
/// ni duplicar RAM.
#[derive(Debug, Clone)]
pub struct ClipSnapshot {
    pub video_packets: Vec<EncodedVideoPacket>,
    /// Cabeceras H.264 SPS + PPS (Annex-B) garantizadas para que cualquier clip sea reproducible
    /// incluso después de que el buffer circular haya descartado el primer keyframe de la sesión.
    pub sps_pps_annexb: Option<Vec<u8>>,
    /// Paquetes de audio AAC-LC recortados exactamente a la ventana de tiempo del video.
    pub audio_packets: Vec<EncodedAudioPacket>,
    /// Configuración del stream AAC (`MF_MT_USER_DATA`, sample rate, bitrate) si hay audio.
    pub audio_info: Option<AudioStreamInfo>,
}

impl ClipSnapshot {
    #[allow(dead_code)]
    pub fn total_bytes(&self) -> usize {
        let v: usize = self.video_packets.iter().map(|p| p.data.len()).sum();
        let a: usize = self.audio_packets.iter().map(|p| p.data.len()).sum();
        v + a
    }

    pub fn duration_secs(&self) -> f64 {
        let (Some(first), Some(last)) = (self.video_packets.first(), self.video_packets.last()) else {
            return 0.0;
        };
        let span = last
            .pts_100ns
            .saturating_sub(first.pts_100ns)
            .saturating_add(last.duration_100ns.max(0));
        span as f64 / 10_000_000.0
    }
}

/// Buffer circular en RAM de paquetes H.264 + AAC comprimidos con índice de Keyframes **O(1)**.
///
/// Garantiza dos invariantes fundamentales:
/// 1. El buffer de video siempre inicia en un Keyframe (IDR) para evitar P-frames huérfanos corruptos.
/// 2. El audio se recorta automáticamente al mismo rango temporal del video y nunca supera `max_bytes`.
pub struct ReplayBuffer {
    video_packets: VecDeque<EncodedVideoPacket>,
    audio_packets: VecDeque<EncodedAudioPacket>,
    /// Secuencia monotónica del primer paquete en `video_packets` (`video_packets[0]`).
    base_seq: u64,
    /// Posiciones (en coordenadas de secuencia) de todos los Keyframes presentes en `video_packets`.
    keyframe_seqs: VecDeque<u64>,
    sps_pps_annexb: Option<Vec<u8>>,
    audio_info: Option<AudioStreamInfo>,
    max_duration_100ns: i64,
    max_bytes: usize,
    total_bytes: usize,
}

impl ReplayBuffer {
    /// Crea un nuevo buffer circular H.264 con duración objetivo y límite máximo de RAM en bytes.
    pub fn new(duration_secs: u32, max_bytes: usize) -> Self {
        Self {
            video_packets: VecDeque::with_capacity(duration_secs as usize * 65),
            audio_packets: VecDeque::with_capacity(duration_secs as usize * 50),
            base_seq: 0,
            keyframe_seqs: VecDeque::with_capacity(duration_secs as usize / 2 + 4),
            sps_pps_annexb: None,
            audio_info: None,
            max_duration_100ns: duration_secs as i64 * 10_000_000,
            max_bytes: max_bytes.max(8 * 1024 * 1024),
            total_bytes: 0,
        }
    }

    /// Registra los metadatos del flujo AAC (`MF_MT_USER_DATA`) para incluir audio en los snapshots.
    pub fn set_audio_info(&mut self, info: AudioStreamInfo) {
        self.audio_info = Some(info);
    }

    /// Inserta un paquete H.264 en el buffer circular y purga GOPs antiguos en O(1) si excede tiempo o RAM.
    pub fn push_video(&mut self, packet: EncodedVideoPacket) {
        if packet.is_keyframe {
            if let Some(headers) = extract_sps_pps_annexb(&packet.data) {
                self.sps_pps_annexb = Some(headers);
            }
        }

        // Si el buffer está vacío, descartamos P-frames iniciales hasta recibir el primer Keyframe.
        if self.video_packets.is_empty() && !packet.is_keyframe {
            return;
        }

        let seq = self.base_seq + self.video_packets.len() as u64;
        if packet.is_keyframe {
            self.keyframe_seqs.push_back(seq);
        }

        self.total_bytes += packet.data.len();
        self.video_packets.push_back(packet);

        self.evict_old_gops();
    }

    /// Inserta un paquete AAC-LC en el buffer circular sincronizado por timestamp QPC (`pts_100ns`).
    pub fn push_audio(&mut self, packet: EncodedAudioPacket) {
        self.total_bytes += packet.data.len();
        self.audio_packets.push_back(packet);
        self.evict_old_audio();
    }

    /// Expulsa GOPs completos desde el inicio del buffer en O(1) usando `keyframe_seqs`.
    fn evict_old_gops(&mut self) {
        let Some(newest_pts) = self.video_packets.back().map(|p| p.pts_100ns) else {
            return;
        };

        let mut evicted_any = false;
        while self.keyframe_seqs.len() >= 2 {
            let next_kf_seq = self.keyframe_seqs[1];
            let cut_idx = (next_kf_seq - self.base_seq) as usize;
            if cut_idx >= self.video_packets.len() {
                break;
            }

            let span_after_cut = newest_pts.saturating_sub(self.video_packets[cut_idx].pts_100ns);
            let exceeds_duration = span_after_cut >= self.max_duration_100ns;
            let exceeds_memory = self.total_bytes > self.max_bytes;

            if exceeds_duration || exceeds_memory {
                for _ in 0..cut_idx {
                    if let Some(removed) = self.video_packets.pop_front() {
                        self.total_bytes = self.total_bytes.saturating_sub(removed.data.len());
                    }
                }
                self.base_seq = next_kf_seq;
                self.keyframe_seqs.pop_front();
                evicted_any = true;
            } else {
                break;
            }
        }

        if evicted_any {
            self.evict_old_audio();
        }
    }

    /// Purga paquetes de audio anteriores al primer keyframe en memoria (con un margen de 500 ms).
    fn evict_old_audio(&mut self) {
        let cutoff_pts = if let Some(first_video) = self.video_packets.front() {
            first_video.pts_100ns.saturating_sub(5_000_000)
        } else if let Some(last_audio) = self.audio_packets.back() {
            last_audio
                .pts_100ns
                .saturating_sub(self.max_duration_100ns + 20_000_000)
        } else {
            return;
        };

        while let Some(front) = self.audio_packets.front() {
            if front.pts_100ns < cutoff_pts {
                let removed = self.audio_packets.pop_front().unwrap();
                self.total_bytes = self.total_bytes.saturating_sub(removed.data.len());
            } else {
                break;
            }
        }
    }

    /// Extrae una instantánea **Zero-Copy** (`Arc<[u8]>`) de los últimos `clip_secs` segundos.
    /// Siempre empieza exactamente en un Keyframe (IDR) e incluye el audio sincronizado en ese rango.
    pub fn snapshot(&self, clip_secs: Option<u32>) -> Option<ClipSnapshot> {
        let newest_pts = self.video_packets.back()?.pts_100ns;
        let window_100ns = clip_secs
            .map(|s| s as i64 * 10_000_000)
            .unwrap_or(self.max_duration_100ns);
        let target_start_pts = newest_pts.saturating_sub(window_100ns);

        // Búsqueda rápida solo sobre el índice de Keyframes (~15–60 entradas).
        let mut start_idx: Option<usize> = None;
        for &kf_seq in &self.keyframe_seqs {
            let idx = (kf_seq - self.base_seq) as usize;
            if let Some(pkt) = self.video_packets.get(idx) {
                if pkt.pts_100ns <= target_start_pts || start_idx.is_none() {
                    start_idx = Some(idx);
                } else {
                    break;
                }
            }
        }

        let start_idx = start_idx?;
        let mut video_packets = Vec::with_capacity(self.video_packets.len() - start_idx);
        for pkt in self.video_packets.iter().skip(start_idx) {
            video_packets.push(pkt.clone());
        }

        if video_packets.is_empty() {
            return None;
        }

        let clip_start_pts = video_packets[0].pts_100ns;
        let last_video = video_packets.last().unwrap();
        let clip_end_pts = last_video
            .pts_100ns
            .saturating_add(last_video.duration_100ns.max(1_000_000));

        // Audio ordenado cronológicamente: usar `partition_point` O(log N) para saltar el inicio.
        let audio_start = self
            .audio_packets
            .partition_point(|a| a.pts_100ns < clip_start_pts);
        let mut audio_packets = Vec::with_capacity(self.audio_packets.len().saturating_sub(audio_start));
        for a in self.audio_packets.iter().skip(audio_start) {
            if a.pts_100ns > clip_end_pts {
                break;
            }
            audio_packets.push(a.clone());
        }

        Some(ClipSnapshot {
            video_packets,
            sps_pps_annexb: self.sps_pps_annexb.clone(),
            audio_packets,
            audio_info: self.audio_info.clone(),
        })
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    #[allow(dead_code)]
    pub fn video_packet_count(&self) -> usize {
        self.video_packets.len()
    }

    #[allow(dead_code)]
    pub fn audio_packet_count(&self) -> usize {
        self.audio_packets.len()
    }

    #[allow(dead_code)]
    pub fn keyframe_count(&self) -> usize {
        self.keyframe_seqs.len()
    }

    pub fn buffered_secs(&self) -> f64 {
        let (Some(first), Some(last)) = (self.video_packets.front(), self.video_packets.back()) else {
            return 0.0;
        };
        last.pts_100ns.saturating_sub(first.pts_100ns) as f64 / 10_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn fake_kf(pts_sec: i64, with_sps_pps: bool) -> EncodedVideoPacket {
        let mut data = Vec::new();
        if with_sps_pps {
            data.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E]); // SPS (type 7)
            data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80]); // PPS (type 8)
        }
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]); // IDR (type 5)
        EncodedVideoPacket {
            data: Arc::from(data),
            pts_100ns: pts_sec * 10_000_000,
            duration_100ns: 10_000_000 / 30,
            is_keyframe: true,
        }
    }

    fn fake_p(pts_100ns: i64) -> EncodedVideoPacket {
        EncodedVideoPacket {
            data: Arc::from([0, 0, 0, 1, 0x41, 0x9A, 0x01]),
            pts_100ns,
            duration_100ns: 10_000_000 / 30,
            is_keyframe: false,
        }
    }

    fn fake_audio(pts_100ns: i64) -> EncodedAudioPacket {
        EncodedAudioPacket {
            data: Arc::from([0x21, 0x10, 0x05, 0xFF]),
            pts_100ns,
            duration_100ns: 213_333,
        }
    }

    #[test]
    fn evicts_on_gop_boundaries_and_syncs_audio() {
        let mut buf = ReplayBuffer::new(4, 64 * 1024 * 1024);

        // Descarta P-frame inicial antes del primer keyframe.
        buf.push_video(fake_p(0));
        assert_eq!(buf.video_packet_count(), 0);
        assert_eq!(buf.keyframe_count(), 0);

        // Insertar 5 GOPs de 2 segundos cada uno (t = 0, 2, 4, 6, 8) + audio cada 500 ms.
        for gop in 0..5 {
            let t = gop * 2;
            buf.push_video(fake_kf(t, gop == 0));
            buf.push_audio(fake_audio(t * 10_000_000));
            buf.push_audio(fake_audio(t * 10_000_000 + 5_000_000));
            buf.push_video(fake_p(t * 10_000_000 + 5_000_000));
            buf.push_audio(fake_audio(t * 10_000_000 + 10_000_000));
            buf.push_video(fake_p(t * 10_000_000 + 10_000_000));
        }

        assert!(buf.keyframe_count() >= 2);
        let snap = buf.snapshot(Some(4)).expect("debería generar snapshot");
        assert!(snap.video_packets[0].is_keyframe);
        assert!(snap.sps_pps_annexb.is_some());
        assert!(snap.duration_secs() >= 4.0);
        assert!(!snap.audio_packets.is_empty());

        // Verificar Zero-Copy real: el Arc del primer paquete del snapshot comparte puntero con el buffer.
        assert!(Arc::ptr_eq(
            &snap.video_packets.last().unwrap().data,
            &buf.video_packets.back().unwrap().data
        ));

        let v_start = snap.video_packets[0].pts_100ns;
        assert!(snap.audio_packets.iter().all(|a| a.pts_100ns >= v_start));
    }
}
