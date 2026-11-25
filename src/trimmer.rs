use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tracing::info;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader,
    MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ANY_STREAM,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_MEDIASOURCE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_TRANSCODE_CONTAINERTYPE, MF_VERSION, MFAudioFormat_AAC,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video,
    MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFTranscodeContainerType_MPEG4,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    eAVEncH264VProfile_High,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HSTRING};

/// Resultado de un recorte o compresión de clip MP4.
#[derive(Debug, Clone)]
pub struct TrimStats {
    pub output_path: PathBuf,
    pub output_bytes: u64,
    pub duration_secs: f64,
    pub elapsed_ms: f64,
}

/// Consulta rápidamente la duración real de un archivo `.mp4` en segundos (~2 ms)
/// usando `IMFSourceReader` sin decodificar frames.
pub fn probe_mp4_duration_secs(path: &Path) -> Result<f64> {
    unsafe { MFStartup(MF_VERSION, 0) }.context("MFStartup (probe) falló")?;
    let res = probe_inner(path);
    unsafe {
        let _ = MFShutdown();
    }
    res
}

fn probe_inner(path: &Path) -> Result<f64> {
    let url = HSTRING::from(path.as_os_str());
    let reader: IMFSourceReader = unsafe { MFCreateSourceReaderFromURL(&url, None) }
        .with_context(|| format!("No se pudo abrir {} con IMFSourceReader", path.display()))?;

    let prop: PROPVARIANT = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .context("No se pudo leer MF_PD_DURATION")?;

    let duration_100ns: u64 = u64::try_from(&prop)
        .or_else(|_| i64::try_from(&prop).map(|v| v.max(0) as u64))
        .context("MF_PD_DURATION no es un entero válido")?;

    Ok(duration_100ns as f64 / 10_000_000.0)
}

/// Recorta un clip `.mp4` entre `[start_secs, end_secs]` en modo **passthrough (sin recodificar)**
/// usando `IMFSourceReader` → `IMFSinkWriter`.
///
/// Conserva la calidad original al 100% (H.264 + AAC) y tarda pocos milisegundos.
/// Nota: el inicio se alinea automáticamente al Keyframe (IDR) más cercano <= `start_secs`.
pub fn trim_mp4_passthrough(
    input_path: &Path,
    output_path: &Path,
    start_secs: f64,
    end_secs: f64,
) -> Result<TrimStats> {
    if end_secs <= start_secs + 0.1 {
        bail!("El tiempo final debe ser mayor al inicial (al menos 0.1 s)");
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let t0 = Instant::now();
    unsafe { MFStartup(MF_VERSION, 0) }.context("MFStartup (trimmer) falló")?;
    let res = trim_inner(input_path, output_path, start_secs, end_secs);
    unsafe {
        let _ = MFShutdown();
    }

    let duration_secs = res?;
    let output_bytes = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    info!(
        input = %input_path.display(),
        output = %output_path.display(),
        start_secs = format!("{start_secs:.2}"),
        end_secs = format!("{end_secs:.2}"),
        duration_secs = format!("{duration_secs:.2}"),
        size_kb = output_bytes / 1024,
        elapsed_ms = format!("{elapsed_ms:.1}"),
        "Clip MP4 recortado sin recodificar"
    );

    Ok(TrimStats {
        output_path: output_path.to_path_buf(),
        output_bytes,
        duration_secs,
        elapsed_ms,
    })
}

/// Comprime un clip `.mp4` (o el rango `[start_secs, end_secs]`) usando la **GPU por hardware**
/// vía Media Foundation para ajustarlo debajo de `target_mb` (ej. `9.5 MB` para Discord gratis).
///
/// Si el presupuesto de bitrate es ajustado para la resolución original (p. ej. 2K largo a <10 MB),
/// reduce automáticamente la resolución a 1080p o 720p para mantener máxima nitidez sin artefactos.
pub fn compress_mp4_to_target_mb(
    input_path: &Path,
    output_path: &Path,
    target_mb: f64,
    start_secs: f64,
    end_secs: f64,
) -> Result<TrimStats> {
    if end_secs <= start_secs + 0.2 {
        bail!("El rango a comprimir debe durar al menos 0.2 segundos");
    }
    if target_mb < 1.0 {
        bail!("El tamaño objetivo debe ser al menos 1 MB");
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let t0 = Instant::now();
    unsafe { MFStartup(MF_VERSION, 0) }.context("MFStartup (compressor) falló")?;
    let res = compress_inner(input_path, output_path, target_mb, start_secs, end_secs);
    unsafe {
        let _ = MFShutdown();
    }

    let duration_secs = res?;
    let output_bytes = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    info!(
        input = %input_path.display(),
        output = %output_path.display(),
        target_mb = format!("{target_mb:.1}"),
        actual_mb = format!("{:.2}", output_bytes as f64 / (1024.0 * 1024.0)),
        duration_secs = format!("{duration_secs:.2}"),
        elapsed_ms = format!("{elapsed_ms:.0}"),
        "Clip MP4 comprimido por hardware (GPU)"
    );

    Ok(TrimStats {
        output_path: output_path.to_path_buf(),
        output_bytes,
        duration_secs,
        elapsed_ms,
    })
}

fn compress_inner(
    input_path: &Path,
    output_path: &Path,
    target_mb: f64,
    start_secs: f64,
    end_secs: f64,
) -> Result<f64> {
    let mut reader_attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut reader_attrs, 2) }?;
    let reader_attrs = reader_attrs.context("MFCreateAttributes (reader) devolvió None")?;
    unsafe {
        reader_attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        reader_attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
    }

    let in_url = HSTRING::from(input_path.as_os_str());
    let reader: IMFSourceReader =
        unsafe { MFCreateSourceReaderFromURL(&in_url, Some(&reader_attrs)) }
            .with_context(|| format!("No se pudo abrir {}", input_path.display()))?;

    // Descubrir pistas nativas de video y audio.
    let mut video_in_idx: Option<u32> = None;
    let mut audio_in_idx: Option<u32> = None;
    let mut src_w = 1920u32;
    let mut src_h = 1080u32;
    let mut src_fps_num = 60u32;
    let mut src_fps_den = 1u32;

    for idx in 0..8u32 {
        let Ok(native_type): windows::core::Result<IMFMediaType> =
            (unsafe { reader.GetNativeMediaType(idx, 0) })
        else {
            break;
        };
        let Ok(major) = (unsafe { native_type.GetGUID(&MF_MT_MAJOR_TYPE) }) else {
            continue;
        };

        if major == MFMediaType_Video && video_in_idx.is_none() {
            video_in_idx = Some(idx);
            if let Ok(packed_size) = unsafe { native_type.GetUINT64(&MF_MT_FRAME_SIZE) } {
                let (w, h) = unpack_u32_pair(packed_size);
                if w > 0 && h > 0 {
                    src_w = w;
                    src_h = h;
                }
            }
            if let Ok(packed_fps) = unsafe { native_type.GetUINT64(&MF_MT_FRAME_RATE) } {
                let (n, d) = unpack_u32_pair(packed_fps);
                if n > 0 && d > 0 {
                    src_fps_num = n;
                    src_fps_den = d;
                }
            }
            unsafe { reader.SetStreamSelection(idx, true) }?;
        } else if major == MFMediaType_Audio && audio_in_idx.is_none() {
            audio_in_idx = Some(idx);
            unsafe { reader.SetStreamSelection(idx, true) }?;
        } else {
            let _ = unsafe { reader.SetStreamSelection(idx, false) };
        }
    }

    let video_in_idx = video_in_idx.context("El archivo MP4 no contiene pista de video")?;

    // Calcular presupuesto exacto de bitrate con 12% de margen de seguridad para contenedor/picos VBR.
    let clip_dur = (end_secs - start_secs).max(0.5);
    let audio_bps: u32 = if audio_in_idx.is_some() { 96_000 } else { 0 };
    let total_budget_bits = target_mb * 1024.0 * 1024.0 * 8.0 * 0.88;
    let raw_video_bps = (total_budget_bits / clip_dur) - audio_bps as f64;
    let video_bitrate_bps = (raw_video_bps as u32).clamp(450_000, 12_000_000);

    // Escalado inteligente cuando el bitrate por píxel es bajo (mejor 720p nítido que 2K pixelado).
    let (out_w, out_h) = choose_compressed_dimensions(src_w, src_h, video_bitrate_bps);

    // Configurar lector de video para entregar NV12 escalado en GPU.
    let reader_nv12 = unsafe { MFCreateMediaType() }?;
    unsafe {
        reader_nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        reader_nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader_nv12.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(out_w, out_h))?;
        reader.SetCurrentMediaType(video_in_idx, None, &reader_nv12)?;
    }

    let mut writer_attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut writer_attrs, 2) }?;
    let writer_attrs = writer_attrs.context("MFCreateAttributes (writer) devolvió None")?;
    unsafe {
        writer_attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        writer_attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }

    let out_url = HSTRING::from(output_path.as_os_str());
    let writer: IMFSinkWriter =
        unsafe { MFCreateSinkWriterFromURL(&out_url, None, Some(&writer_attrs)) }
            .with_context(|| format!("No se pudo crear {}", output_path.display()))?;

    // Configurar pista de salida H.264 con el bitrate objetivo + entrada NV12.
    let h264_out = unsafe { MFCreateMediaType() }?;
    unsafe {
        h264_out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        h264_out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        h264_out.SetUINT32(&MF_MT_AVG_BITRATE, video_bitrate_bps)?;
        h264_out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        h264_out.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(out_w, out_h))?;
        h264_out.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(src_fps_num, src_fps_den))?;
        h264_out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))?;
        h264_out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
    }

    let video_out_idx = unsafe { writer.AddStream(&h264_out) }
        .context("IMFSinkWriter::AddStream (H264 compress) falló")?;

    let nv12_in = unsafe { MFCreateMediaType() }?;
    unsafe {
        nv12_in.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        nv12_in.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        nv12_in.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        nv12_in.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(out_w, out_h))?;
        nv12_in.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(src_fps_num, src_fps_den))?;
        nv12_in.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32_pair(1, 1))?;
        writer.SetInputMediaType(video_out_idx, &nv12_in, None)?;
    }

    // Configurar audio: recodificar a AAC 96 kbps (48 kHz estéreo) para maximizar presupuesto de video.
    let audio_out_idx = if let Some(a_in) = audio_in_idx {
        let pcm_type = unsafe { MFCreateMediaType() }?;
        unsafe {
            pcm_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            pcm_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            pcm_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            pcm_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
            pcm_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            pcm_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            pcm_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 48_000 * 4)?;
            reader.SetCurrentMediaType(a_in, None, &pcm_type)?;
        }

        let aac_out = unsafe { MFCreateMediaType() }?;
        unsafe {
            aac_out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            aac_out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            aac_out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            aac_out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
            aac_out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            aac_out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 12_000)?; // 96 kbps
            aac_out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            aac_out.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
        }

        let out_idx = unsafe { writer.AddStream(&aac_out) }
            .context("IMFSinkWriter::AddStream (AAC 96k) falló")?;
        unsafe { writer.SetInputMediaType(out_idx, &pcm_type, None) }
            .context("IMFSinkWriter::SetInputMediaType (PCM) falló")?;
        Some(out_idx)
    } else {
        None
    };

    let start_100ns = (start_secs.max(0.0) * 10_000_000.0) as i64;
    let end_100ns = (end_secs * 10_000_000.0) as i64;

    if start_100ns > 0 {
        let pos = PROPVARIANT::from(start_100ns);
        let _ = unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &pos) };
    }

    unsafe { writer.BeginWriting() }.context("IMFSinkWriter::BeginWriting (compress) falló")?;

    let mut base_pts: Option<i64> = None;
    let mut last_v_pts: i64 = -1;
    let mut last_a_pts: i64 = -1;
    let mut max_rel_pts: i64 = 0;
    let mut video_done = false;
    let mut audio_done = audio_out_idx.is_none();

    while !(video_done && audio_done) {
        let mut actual_stream = 0u32;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample_opt: Option<IMFSample> = None;

        unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut actual_stream),
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample_opt),
            )
        }
        .context("IMFSourceReader::ReadSample (compress) falló")?;

        if (flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            if actual_stream == video_in_idx {
                video_done = true;
            } else if Some(actual_stream) == audio_in_idx {
                audio_done = true;
            } else {
                break;
            }
            continue;
        }

        let Some(sample) = sample_opt else {
            continue;
        };

        let is_video = actual_stream == video_in_idx;
        if timestamp < start_100ns {
            continue;
        }
        if timestamp > end_100ns {
            if is_video {
                video_done = true;
            } else {
                audio_done = true;
            }
            continue;
        }

        let origin = match base_pts {
            Some(b) => b,
            None => {
                if !is_video {
                    continue;
                }
                base_pts = Some(timestamp);
                timestamp
            }
        };

        if timestamp < origin {
            continue;
        }

        let mut rel_pts = timestamp - origin;
        let dur = unsafe { sample.GetSampleDuration() }.unwrap_or(0);

        if is_video {
            if rel_pts <= last_v_pts {
                rel_pts = last_v_pts + 1;
            }
            unsafe { sample.SetSampleTime(rel_pts) }?;
            unsafe { writer.WriteSample(video_out_idx, &sample) }
                .context("IMFSinkWriter::WriteSample (video compress) falló")?;
            last_v_pts = rel_pts;
            max_rel_pts = max_rel_pts.max(rel_pts + dur.max(0));
        } else if let Some(a_out) = audio_out_idx {
            if rel_pts <= last_a_pts {
                rel_pts = last_a_pts + 1;
            }
            unsafe { sample.SetSampleTime(rel_pts) }?;
            unsafe { writer.WriteSample(a_out, &sample) }
                .context("IMFSinkWriter::WriteSample (audio compress) falló")?;
            last_a_pts = rel_pts;
        }
    }

    if base_pts.is_none() {
        bail!("No se pudieron leer frames en el rango seleccionado para comprimir");
    }

    unsafe { writer.Finalize() }.context("IMFSinkWriter::Finalize (compress) falló")?;

    Ok(max_rel_pts as f64 / 10_000_000.0)
}

fn choose_compressed_dimensions(src_w: u32, src_h: u32, video_bitrate_bps: u32) -> (u32, u32) {
    let target_h = if video_bitrate_bps < 2_400_000 && src_h > 720 {
        720
    } else if video_bitrate_bps < 5_000_000 && src_h > 1080 {
        1080
    } else {
        src_h
    };

    if target_h >= src_h {
        return (src_w & !1, src_h & !1);
    }

    let scaled_w = ((src_w as f64 * target_h as f64) / src_h as f64).round() as u32;
    ((scaled_w & !1).max(320), (target_h & !1).max(240))
}

fn trim_inner(
    input_path: &Path,
    output_path: &Path,
    start_secs: f64,
    end_secs: f64,
) -> Result<f64> {
    let in_url = HSTRING::from(input_path.as_os_str());
    let reader: IMFSourceReader = unsafe { MFCreateSourceReaderFromURL(&in_url, None) }
        .with_context(|| format!("No se pudo abrir {}", input_path.display()))?;

    let mut attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attrs, 2) }?;
    let attrs = attrs.context("MFCreateAttributes devolvió None")?;
    unsafe {
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }

    let out_url = HSTRING::from(output_path.as_os_str());
    let writer: IMFSinkWriter = unsafe { MFCreateSinkWriterFromURL(&out_url, None, Some(&attrs)) }
        .with_context(|| format!("No se pudo crear {}", output_path.display()))?;

    let mut stream_map: HashMap<u32, u32> = HashMap::new();
    let mut video_in_idx: Option<u32> = None;
    let mut audio_in_idx: Option<u32> = None;

    for idx in 0..8u32 {
        let Ok(native_type): windows::core::Result<IMFMediaType> =
            (unsafe { reader.GetNativeMediaType(idx, 0) })
        else {
            break;
        };

        let Ok(major) = (unsafe { native_type.GetGUID(&MF_MT_MAJOR_TYPE) }) else {
            continue;
        };

        let should_include = if major == MFMediaType_Video && video_in_idx.is_none() {
            video_in_idx = Some(idx);
            true
        } else if major == MFMediaType_Audio && audio_in_idx.is_none() {
            audio_in_idx = Some(idx);
            true
        } else {
            false
        };

        if should_include {
            unsafe { reader.SetStreamSelection(idx, true) }?;
            let out_idx = unsafe { writer.AddStream(&native_type) }
                .context("IMFSinkWriter::AddStream en trimmer falló")?;
            unsafe { writer.SetInputMediaType(out_idx, &native_type, None) }
                .context("IMFSinkWriter::SetInputMediaType en trimmer falló")?;
            stream_map.insert(idx, out_idx);
        } else {
            let _ = unsafe { reader.SetStreamSelection(idx, false) };
        }
    }

    let video_in_idx = video_in_idx.context("El archivo MP4 no contiene pista de video")?;

    let start_100ns = (start_secs.max(0.0) * 10_000_000.0) as i64;
    let end_100ns = (end_secs * 10_000_000.0) as i64;

    if start_100ns > 0 {
        let pos = PROPVARIANT::from(start_100ns);
        unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &pos) }
            .context("IMFSourceReader::SetCurrentPosition falló")?;
    }

    unsafe { writer.BeginWriting() }.context("IMFSinkWriter::BeginWriting (trimmer) falló")?;

    let mut base_pts: Option<i64> = None;
    let mut last_written_pts: HashMap<u32, i64> = HashMap::new();
    let mut max_rel_pts: i64 = 0;
    let mut video_done = false;
    let mut audio_done = audio_in_idx.is_none();

    while !(video_done && audio_done) {
        let mut actual_stream = 0u32;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample_opt: Option<IMFSample> = None;

        unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut actual_stream),
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample_opt),
            )
        }
        .context("IMFSourceReader::ReadSample falló")?;

        if (flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            if actual_stream == video_in_idx {
                video_done = true;
            } else if Some(actual_stream) == audio_in_idx {
                audio_done = true;
            } else {
                break;
            }
            continue;
        }

        let Some(sample) = sample_opt else {
            continue;
        };
        let Some(&out_stream) = stream_map.get(&actual_stream) else {
            continue;
        };

        let is_video = actual_stream == video_in_idx;

        if timestamp > end_100ns {
            if is_video {
                video_done = true;
            } else {
                audio_done = true;
            }
            continue;
        }

        let origin = match base_pts {
            Some(b) => b,
            None => {
                if !is_video {
                    continue;
                }
                let clean =
                    unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
                if !clean {
                    continue;
                }
                base_pts = Some(timestamp);
                timestamp
            }
        };

        if timestamp < origin {
            continue;
        }

        let mut rel_pts = timestamp - origin;
        if let Some(&prev) = last_written_pts.get(&out_stream) {
            if rel_pts <= prev {
                rel_pts = prev + 1;
            }
        }

        let dur = unsafe { sample.GetSampleDuration() }.unwrap_or(0);
        unsafe { sample.SetSampleTime(rel_pts) }?;
        unsafe { writer.WriteSample(out_stream, &sample) }
            .context("IMFSinkWriter::WriteSample (trimmer) falló")?;

        last_written_pts.insert(out_stream, rel_pts);
        if is_video {
            max_rel_pts = max_rel_pts.max(rel_pts + dur.max(0));
        }
    }

    if base_pts.is_none() {
        bail!("No se encontró ningún keyframe de video en el rango seleccionado");
    }

    unsafe { writer.Finalize() }.context("IMFSinkWriter::Finalize (trimmer) falló")?;

    Ok(max_rel_pts as f64 / 10_000_000.0)
}

/// Construye un nombre de salida único (`clip_<suffix>.mp4`, o `_<suffix>2.mp4` si ya existe).
pub fn suggested_output_with_suffix(input: &Path, suffix: &str) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".to_string());

    let first = parent.join(format!("{stem}_{suffix}.mp4"));
    if !first.exists() {
        return first;
    }

    for i in 2..1000 {
        let candidate = parent.join(format!("{stem}_{suffix}{i}.mp4"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

pub fn suggested_trim_output(input: &Path) -> PathBuf {
    suggested_output_with_suffix(input, "trim")
}

#[inline]
const fn pack_u32_pair(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

#[inline]
const fn unpack_u32_pair(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_sample_mp4() -> Option<PathBuf> {
        let clips_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("clips");
        let entries = std::fs::read_dir(&clips_dir).ok()?;
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|ext| ext == "mp4"))
    }

    #[test]
    fn trims_existing_sample_clip_if_present() {
        let Some(mp4) = find_sample_mp4() else {
            return;
        };

        let dur = probe_mp4_duration_secs(&mp4).expect("probe_mp4_duration_secs falló");
        assert!(dur > 1.0, "duración inesperada: {dur}");

        let out = std::env::temp_dir().join("bals_trim_unit_test.mp4");
        let _ = std::fs::remove_file(&out);

        let end = (dur - 0.5).max(1.5).min(dur);
        let stats = trim_mp4_passthrough(&mp4, &out, 0.5, end).expect("trim_mp4_passthrough falló");
        assert!(stats.output_bytes > 1024);
        assert!(stats.duration_secs > 0.5);

        let probed_out = probe_mp4_duration_secs(&out).expect("probe en archivo recortado falló");
        assert!(probed_out > 0.5 && probed_out <= dur + 0.5);

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn compresses_existing_sample_clip_under_target() {
        let Some(mp4) = find_sample_mp4() else {
            return;
        };

        let dur = probe_mp4_duration_secs(&mp4).expect("probe falló");
        let out = std::env::temp_dir().join("bals_compress_unit_test.mp4");
        let _ = std::fs::remove_file(&out);

        let stats = compress_mp4_to_target_mb(&mp4, &out, 2.0, 0.0, dur)
            .expect("compress_mp4_to_target_mb falló");
        assert!(stats.output_bytes > 10_000);
        // Debe quedar por debajo del objetivo (2.0 MB + tolerancia pequeña de VBR/contenedor).
        assert!(
            stats.output_bytes < 2_500_000,
            "archivo comprimido demasiado grande: {} bytes",
            stats.output_bytes
        );

        let probed_out = probe_mp4_duration_secs(&out).expect("probe en archivo comprimido falló");
        assert!(probed_out > 1.0);

        let _ = std::fs::remove_file(&out);
    }
}
