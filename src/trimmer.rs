use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tracing::info;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MF_MT_MAJOR_TYPE,
    MF_PD_DURATION, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ANY_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM, MF_TRANSCODE_CONTAINERTYPE,
    MF_VERSION, MFCreateAttributes, MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL,
    MFMediaType_Audio, MFMediaType_Video, MFSampleExtension_CleanPoint, MFShutdown, MFStartup,
    MFTranscodeContainerType_MPEG4,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HSTRING};

/// Resultado de un recorte de clip MP4 sin recodificar.
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

    // Descubrir streams nativos (video H.264 + audio AAC opcional) y mapearlos en passthrough.
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
            // Passthrough real: mismo tipo comprimido nativo en origen y destino.
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

    // Posicionar el lector cerca del segundo de inicio (Windows alinea al keyframe previo).
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

        // Esperar al primer Keyframe (IDR) de video para arrancar el clip recortado limpiamente.
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

/// Construye el nombre de salida sugerido (`clip_trim.mp4`, o `_trim2.mp4` si ya existe).
pub fn suggested_trim_output(input: &Path) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".to_string());

    let first = parent.join(format!("{stem}_trim.mp4"));
    if !first.exists() {
        return first;
    }

    for i in 2..1000 {
        let candidate = parent.join(format!("{stem}_trim{i}.mp4"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_existing_sample_clip_if_present() {
        let clips_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("clips");
        let Ok(entries) = std::fs::read_dir(&clips_dir) else {
            return;
        };
        let Some(mp4) = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|ext| ext == "mp4"))
        else {
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
}
