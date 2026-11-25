use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tracing::info;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaBuffer, IMFSample, IMFSinkWriter, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION,
    MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_USER_DATA,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_TRANSCODE_CONTAINERTYPE, MF_VERSION,
    MFAudioFormat_AAC, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video, MFSampleExtension_CleanPoint,
    MFShutdown, MFStartup, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
    MFVideoInterlace_Progressive, eAVEncH264VProfile_High,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::HSTRING;

use crate::buffer::ClipSnapshot;
use crate::config::VideoConfig;
use crate::encode::annexb_contains_sps;

/// Estadísticas de un archivo MP4 recién guardado.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WriteStats {
    pub path: PathBuf,
    pub file_bytes: u64,
    pub video_packets_written: usize,
    pub audio_packets_written: usize,
    pub clip_duration_secs: f64,
    pub mux_elapsed_ms: f64,
}

/// Escribe un `ClipSnapshot` (H.264 + AAC-LC) directamente a disco como archivo `.mp4`
/// mediante `IMFSinkWriter` en modo **passthrough (sin recodificar)**.
pub fn write_mp4_clip(
    output_path: &Path,
    snapshot: &ClipSnapshot,
    video_config: VideoConfig,
) -> Result<WriteStats> {
    if snapshot.video_packets.is_empty() {
        bail!("No hay paquetes de video en el snapshot para escribir el clip");
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "No se pudo crear el directorio de salida {}",
                parent.display()
            )
        })?;
    }

    let t0 = Instant::now();

    unsafe { MFStartup(MF_VERSION, 0) }.context("MFStartup en muxer MP4 falló")?;

    let write_res = write_mp4_inner(output_path, snapshot, video_config);

    unsafe {
        let _ = MFShutdown();
    }

    let (video_packets_written, audio_packets_written) = write_res?;
    let file_bytes = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    let mux_elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let clip_duration_secs = snapshot.duration_secs();

    info!(
        path = %output_path.display(),
        size_kb = file_bytes / 1024,
        video_packets = video_packets_written,
        audio_packets = audio_packets_written,
        duration_s = format!("{clip_duration_secs:.2}"),
        mux_ms = format!("{mux_elapsed_ms:.1}"),
        "Clip MP4 guardado (passthrough H.264 + AAC sin recodificar)"
    );

    Ok(WriteStats {
        path: output_path.to_path_buf(),
        file_bytes,
        video_packets_written,
        audio_packets_written,
        clip_duration_secs,
        mux_elapsed_ms,
    })
}

fn write_mp4_inner(
    output_path: &Path,
    snapshot: &ClipSnapshot,
    video_config: VideoConfig,
) -> Result<(usize, usize)> {
    let mut attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attrs, 2) }.context("MFCreateAttributes falló")?;
    let attrs = attrs.context("MFCreateAttributes devolvió None")?;
    unsafe {
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }

    let url = HSTRING::from(output_path.as_os_str());
    let sink_writer: IMFSinkWriter = unsafe { MFCreateSinkWriterFromURL(&url, None, Some(&attrs)) }
        .with_context(|| {
            format!(
                "MFCreateSinkWriterFromURL falló para {}",
                output_path.display()
            )
        })?;

    // 1. Pista de video H.264 (passthrough).
    let (width, height) = video_config.resolution.dimensions();
    let fps = video_config.fps.as_u32();

    let h264_type = unsafe { MFCreateMediaType() }.context("MFCreateMediaType (H264) falló")?;
    unsafe {
        h264_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        h264_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        h264_type.SetUINT32(&MF_MT_AVG_BITRATE, video_config.bitrate_bps)?;
        h264_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        h264_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))?;
        h264_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(fps, 1))?;
        h264_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))?;
        h264_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;

        if let Some(sps_pps) = &snapshot.sps_pps_annexb {
            h264_type.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, sps_pps)?;
        }
    }

    let video_stream_idx = unsafe { sink_writer.AddStream(&h264_type) }
        .context("IMFSinkWriter::AddStream (H.264) falló")?;
    unsafe { sink_writer.SetInputMediaType(video_stream_idx, &h264_type, None) }
        .context("IMFSinkWriter::SetInputMediaType (passthrough H.264) falló")?;

    // 2. Pista de audio AAC-LC (passthrough) si el snapshot contiene audio.
    let audio_stream_idx = if let (Some(audio_info), false) =
        (&snapshot.audio_info, snapshot.audio_packets.is_empty())
    {
        let aac_type = unsafe { MFCreateMediaType() }.context("MFCreateMediaType (AAC) falló")?;
        unsafe {
            aac_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            aac_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            aac_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            aac_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, audio_info.sample_rate)?;
            aac_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, audio_info.channels as u32)?;
            aac_type.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                audio_info.bitrate_bps / 8,
            )?;
            aac_type.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            aac_type.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            aac_type.SetBlob(&MF_MT_USER_DATA, &audio_info.user_data)?;
        }

        let idx = unsafe { sink_writer.AddStream(&aac_type) }
            .context("IMFSinkWriter::AddStream (AAC) falló")?;
        unsafe { sink_writer.SetInputMediaType(idx, &aac_type, None) }
            .context("IMFSinkWriter::SetInputMediaType (passthrough AAC) falló")?;
        Some(idx)
    } else {
        None
    };

    unsafe { sink_writer.BeginWriting() }.context("IMFSinkWriter::BeginWriting falló")?;

    // 3. Intercalar video y audio en orden cronológico estricto desde `base_pts` (t = 0).
    let base_pts = snapshot.video_packets[0].pts_100ns;
    let nominal_v_dur = video_config.fps.frame_duration_100ns();

    let mut v_idx = 0usize;
    let mut a_idx = 0usize;
    let mut last_v_pts: i64 = -1;
    let mut last_a_pts: i64 = -1;
    let mut v_written = 0usize;
    let mut a_written = 0usize;

    let total_v = snapshot.video_packets.len();
    let total_a = if audio_stream_idx.is_some() {
        snapshot.audio_packets.len()
    } else {
        0
    };

    while v_idx < total_v || a_idx < total_a {
        let next_v_pts = snapshot
            .video_packets
            .get(v_idx)
            .map(|p| p.pts_100ns.saturating_sub(base_pts).max(0));
        let next_a_pts = if audio_stream_idx.is_some() {
            snapshot
                .audio_packets
                .get(a_idx)
                .map(|p| p.pts_100ns.saturating_sub(base_pts).max(0))
        } else {
            None
        };

        let write_video = match (next_v_pts, next_a_pts) {
            (Some(vp), Some(ap)) => vp <= ap,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };

        if write_video {
            let pkt = &snapshot.video_packets[v_idx];
            let mut rel_pts = pkt.pts_100ns.saturating_sub(base_pts).max(0);
            if rel_pts <= last_v_pts {
                rel_pts = last_v_pts + 1;
            }

            let duration = if let Some(next_pkt) = snapshot.video_packets.get(v_idx + 1) {
                let next_rel = next_pkt.pts_100ns.saturating_sub(base_pts).max(rel_pts + 1);
                (next_rel - rel_pts).clamp(10_000, 10_000_000)
            } else {
                pkt.duration_100ns.max(nominal_v_dur)
            };

            let payload: std::borrow::Cow<'_, [u8]> =
                if v_idx == 0 && !annexb_contains_sps(&pkt.data) {
                    if let Some(sps_pps) = &snapshot.sps_pps_annexb {
                        let mut combined = Vec::with_capacity(sps_pps.len() + pkt.data.len());
                        combined.extend_from_slice(sps_pps);
                        combined.extend_from_slice(&pkt.data);
                        std::borrow::Cow::Owned(combined)
                    } else {
                        std::borrow::Cow::Borrowed(&pkt.data)
                    }
                } else {
                    std::borrow::Cow::Borrowed(&pkt.data)
                };

            let sample = create_memory_sample(&payload, rel_pts, duration, pkt.is_keyframe)?;
            unsafe { sink_writer.WriteSample(video_stream_idx, &sample) }
                .with_context(|| format!("IMFSinkWriter::WriteSample (video #{v_idx}) falló"))?;

            last_v_pts = rel_pts;
            v_idx += 1;
            v_written += 1;
        } else if let Some(a_stream) = audio_stream_idx {
            let pkt = &snapshot.audio_packets[a_idx];
            let mut rel_pts = pkt.pts_100ns.saturating_sub(base_pts).max(0);
            if rel_pts <= last_a_pts {
                rel_pts = last_a_pts + 1;
            }
            let duration = pkt.duration_100ns.max(10_000);

            let sample = create_memory_sample(&pkt.data, rel_pts, duration, true)?;
            unsafe { sink_writer.WriteSample(a_stream, &sample) }
                .with_context(|| format!("IMFSinkWriter::WriteSample (audio #{a_idx}) falló"))?;

            last_a_pts = rel_pts;
            a_idx += 1;
            a_written += 1;
        }
    }

    unsafe { sink_writer.Finalize() }.context("IMFSinkWriter::Finalize falló")?;

    Ok((v_written, a_written))
}

fn create_memory_sample(
    bytes: &[u8],
    pts_100ns: i64,
    duration_100ns: i64,
    is_keyframe: bool,
) -> Result<IMFSample> {
    let sample = unsafe { MFCreateSample() }.context("MFCreateSample falló")?;
    let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(bytes.len() as u32) }
        .context("MFCreateMemoryBuffer falló")?;

    let mut dst_ptr: *mut u8 = std::ptr::null_mut();
    unsafe { buffer.Lock(&mut dst_ptr, None, None) }.context("IMFMediaBuffer::Lock falló")?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(pts_100ns)?;
        sample.SetSampleDuration(duration_100ns)?;
        if is_keyframe {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }
    }

    Ok(sample)
}

/// Genera una ruta única con fecha y hora local (`bals_YYYYMMDD_HHMMSS.mp4`).
pub fn generate_clip_path(output_dir: &Path) -> PathBuf {
    let st = unsafe { GetLocalTime() };
    let filename = format!(
        "bals_{:04}{:02}{:02}_{:02}{:02}{:02}.mp4",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    );
    output_dir.join(filename)
}

#[inline]
const fn pack_u32_pair(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}
