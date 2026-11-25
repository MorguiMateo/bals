use anyhow::{Context, Result};
use tracing::debug;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::core::Interface;

use crate::d3d::SharedD3DDevice;

const NV12_POOL_SIZE: usize = 8;

struct Nv12Slot {
    texture: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
}

/// Conversor y escalador 100% en GPU (`BGRA8` → `NV12`) usando `ID3D11VideoProcessor`.
///
/// Mantiene un pool circular de texturas `NV12` en VRAM para evitar asignaciones por frame
/// y no sobrescribir superficies mientras el encoder asíncrono aún las está leyendo.
pub struct GpuNv12Converter {
    d3d: SharedD3DDevice,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    fps: u32,
    pool: Vec<Nv12Slot>,
    next_slot: usize,
}

impl GpuNv12Converter {
    pub fn new(
        d3d: &SharedD3DDevice,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        fps: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = d3d
            .device
            .cast()
            .context("La GPU/driver no expone ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = d3d
            .context
            .cast()
            .context("La GPU/driver no expone ID3D11VideoContext")?;

        let (enumerator, processor) = create_processor(
            &video_device,
            &video_context,
            input_width,
            input_height,
            output_width,
            output_height,
            fps,
        )?;

        let pool = create_nv12_pool(d3d, &video_device, &enumerator, output_width, output_height)?;

        debug!(
            in_w = input_width,
            in_h = input_height,
            out_w = output_width,
            out_h = output_height,
            pool_size = pool.len(),
            "GpuNv12Converter inicializado (BGRA -> NV12 + escalado en VRAM)"
        );

        Ok(Self {
            d3d: d3d.clone(),
            video_device,
            video_context,
            enumerator,
            processor,
            input_width,
            input_height,
            output_width,
            output_height,
            fps,
            pool,
            next_slot: 0,
        })
    }

    /// Convierte y escala una textura `BGRA` en GPU a una textura `NV12` del pool (Zero-Copy).
    pub fn convert(&mut self, bgra_texture: &ID3D11Texture2D, src_width: u32, src_height: u32) -> Result<ID3D11Texture2D> {
        // Si cambió el tamaño de entrada (p. ej. el usuario cambió resolución del juego),
        // recreamos el enumerador/procesador manteniendo el tamaño de salida objetivo.
        if src_width != self.input_width || src_height != self.input_height {
            let (enumerator, processor) = create_processor(
                &self.video_device,
                &self.video_context,
                src_width,
                src_height,
                self.output_width,
                self.output_height,
                self.fps,
            )?;
            self.pool = create_nv12_pool(
                &self.d3d,
                &self.video_device,
                &enumerator,
                self.output_width,
                self.output_height,
            )?;
            self.enumerator = enumerator;
            self.processor = processor;
            self.input_width = src_width;
            self.input_height = src_height;
            self.next_slot = 0;
        }

        let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };

        let mut input_view = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                bgra_texture,
                &self.enumerator,
                &in_view_desc,
                Some(&mut input_view),
            )
        }
        .context("CreateVideoProcessorInputView falló")?;

        let slot = &self.pool[self.next_slot];
        self.next_slot = (self.next_slot + 1) % self.pool.len();

        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            pInputSurface: std::mem::ManuallyDrop::new(input_view),
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        }];

        let blt_res = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, &slot.output_view, 0, &streams)
        };

        // Liberar explícitamente el InputView contenido en ManuallyDrop para evitar fugas COM/VRAM.
        unsafe {
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
        }

        blt_res.context("VideoProcessorBlt (BGRA -> NV12) falló")?;

        // Enviar inmediatamente la cola de comandos D3D11 a la GPU para garantizar que la escritura
        // en la textura NV12 quede encolada ANTES de que el encoder hardware (VCE/NVENC/QSV) la lea.
        unsafe {
            self.d3d.context.Flush();
        }

        Ok(slot.texture.clone())
    }
}

fn create_processor(
    video_device: &ID3D11VideoDevice,
    video_context: &ID3D11VideoContext,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    fps: u32,
) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor)> {
    let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        },
        InputWidth: input_width,
        InputHeight: input_height,
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        },
        OutputWidth: output_width,
        OutputHeight: output_height,
        Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    };

    let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content_desc) }
        .context("CreateVideoProcessorEnumerator falló")?;
    let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
        .context("CreateVideoProcessor falló")?;

    // Rectángulo de origen completo y destino preservando relación de aspecto (letterbox limpio).
    let src_rect = RECT {
        left: 0,
        top: 0,
        right: input_width as i32,
        bottom: input_height as i32,
    };
    let dst_rect = fit_aspect_ratio(input_width, input_height, output_width, output_height);

    // Configurar espacio de color: entrada RGB rango completo (0-255) → salida YCbCr Rec.709 (HD/2K).
    let mut in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
    in_cs._bitfield = 0; // Usage=0 (Playback), RGB_Range=0 (Full 0-255)

    let mut out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
    // Bit 2 = YCbCr_Matrix (1 = BT.709), Bit 4 = Nominal_Range (Studio/Video range estándar H.264)
    out_cs._bitfield = 1 << 2;

    unsafe {
        video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src_rect));
        video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst_rect));
        video_context.VideoProcessorSetOutputTargetRect(
            &processor,
            true,
            Some(&RECT {
                left: 0,
                top: 0,
                right: output_width as i32,
                bottom: output_height as i32,
            }),
        );
        video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs);
        video_context.VideoProcessorSetOutputColorSpace(&processor, &out_cs);
    }

    Ok((enumerator, processor))
}

fn create_nv12_pool(
    d3d: &SharedD3DDevice,
    video_device: &ID3D11VideoDevice,
    enumerator: &ID3D11VideoProcessorEnumerator,
    width: u32,
    height: u32,
) -> Result<Vec<Nv12Slot>> {
    let tex_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };

    let mut pool = Vec::with_capacity(NV12_POOL_SIZE);
    for _ in 0..NV12_POOL_SIZE {
        let mut texture = None;
        unsafe { d3d.device.CreateTexture2D(&tex_desc, None, Some(&mut texture)) }
            .context("No se pudo crear textura NV12 en VRAM")?;
        let texture = texture.context("CreateTexture2D devolvió None")?;

        let mut output_view = None;
        unsafe {
            video_device.CreateVideoProcessorOutputView(
                &texture,
                enumerator,
                &out_view_desc,
                Some(&mut output_view),
            )
        }
        .context("CreateVideoProcessorOutputView falló para NV12")?;
        let output_view = output_view.context("CreateVideoProcessorOutputView devolvió None")?;

        pool.push(Nv12Slot {
            texture,
            output_view,
        });
    }

    Ok(pool)
}

/// Calcula un rectángulo centrado dentro de `(out_w, out_h)` preservando la relación de aspecto.
fn fit_aspect_ratio(in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> RECT {
    if in_w == 0 || in_h == 0 || (in_w * out_h == out_w * in_h) {
        return RECT {
            left: 0,
            top: 0,
            right: out_w as i32,
            bottom: out_h as i32,
        };
    }

    let in_aspect = in_w as f64 / in_h as f64;
    let out_aspect = out_w as f64 / out_h as f64;

    let (w, h) = if in_aspect > out_aspect {
        let w = out_w as f64;
        let h = (w / in_aspect).round();
        (w as i32, h as i32)
    } else {
        let h = out_h as f64;
        let w = (h * in_aspect).round();
        (w as i32, h as i32)
    };

    // Alinear a múltiplos de 2 para submuestreo chroma NV12 (4:2:0).
    let w = w & !1;
    let h = h & !1;
    let left = ((out_w as i32 - w) / 2) & !1;
    let top = ((out_h as i32 - h) / 2) & !1;

    RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}
