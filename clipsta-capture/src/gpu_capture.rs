//! Clipsta Lite GPU capture pipeline with DEDICATED ENCODER THREAD:
//! WGC → BGRA D3D11 texture → ID3D11VideoProcessor (scale + BGRA→NV12)
//! → Send NV12 pool texture index to encoder thread via mpsc channel
//! → Encoder thread: blocking GetEvent loop on async MFT
//!   - METransformNeedInput (21): create DXGI surface buffer, ProcessInput
//!   - METransformHaveOutput (22): ProcessOutput, NAL keyframe detect, push to ring
//! → EncodedMediaRing (in-memory H.264 + PCM audio)
//! → keyframe-aligned slice on save → MF Sink Writer → MP4

use std::collections::VecDeque;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, Receiver};
use std::sync::Arc;
use std::thread;

use anyhow::{Context as AnyhowContext, Result};
use parking_lot::Mutex;
use serde::Serialize;

use windows::core::{Interface, HSTRING};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use crate::audio::WasapiCapture;

// ── Constants ─────────────────────────────────────────────────────────────────

const MF_VERSION: u32 = 0x0002_0070;
const AUDIO_SAMPLE_RATE: u32 = 48000;
const AUDIO_CHANNELS: u32 = 2;
const AUDIO_BITS_PER_SAMPLE: u32 = 16;
const AUDIO_BLOCK_ALIGN: u32 = AUDIO_CHANNELS * (AUDIO_BITS_PER_SAMPLE / 8);

/// Output dimensions: 1280x720 (16-pixel aligned, matches ShadowPlay)
const OUTPUT_WIDTH: u32 = 1280;
const OUTPUT_HEIGHT: u32 = 720;

/// Maximum ring buffer duration in seconds (5 minutes — matches Medal, closes gap with ShadowPlay)
const MAX_RING_SECONDS: u32 = 300;

/// NV12 pool size for video processor output.
/// 16 textures balances encoder headroom with VRAM usage.
/// At 1080p: 16 × 1920×1088×1.5 = ~50MB VRAM (matches ShadowPlay's footprint).
const NV12_POOL_SIZE: usize = 16;

fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

/// Message sent from WGC callback to the dedicated encoder thread
struct FrameMsg {
    texture_index: usize,
    pts_100ns: i64,
    duration_100ns: i64,
}

// ── D3D11 Device Creation ─────────────────────────────────────────────────────

/// Find the DXGI adapter that owns the given monitor (hybrid-GPU laptop fix).
unsafe fn find_adapter_for_monitor(hmon: HMONITOR) -> Option<IDXGIAdapter1> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
    let mut i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(i) {
            Ok(a) => a,
            Err(_) => return None,
        };
        i += 1;
        let mut j = 0u32;
        loop {
            let output = match adapter.EnumOutputs(j) {
                Ok(o) => o,
                Err(_) => break,
            };
            j += 1;
            if let Ok(desc) = output.GetDesc() {
                if desc.Monitor == hmon {
                    return Some(adapter);
                }
            }
        }
    }
}

/// Create a D3D11 device with VIDEO_SUPPORT for VP and encoder.
unsafe fn create_d3d11_device(
    adapter: Option<&IDXGIAdapter1>,
) -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;

    use windows::Win32::Foundation::HMODULE;

    let adapter_owned: Option<IDXGIAdapter> = match adapter {
        Some(a) => Some(a.cast()?),
        None => None,
    };
    let driver_type = if adapter_owned.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };

    D3D11CreateDevice(
        adapter_owned.as_ref(),
        driver_type,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        Some(&[D3D_FEATURE_LEVEL(0xb100), D3D_FEATURE_LEVEL(0xb000)]),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    )?;

    let device = device.context("D3D11 device")?;
    let context = context.context("D3D11 context")?;

    // Enable multithreaded access
    let mt: ID3D11Multithread = device.cast()?;
    let _ = mt.SetMultithreadProtected(true);

    // Create WinRT IDirect3DDevice for WGC
    let dxgi_device: IDXGIDevice = device.cast()?;
    let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)?;
    let winrt_device: IDirect3DDevice = inspectable.cast()?;

    Ok((device, context, winrt_device))
}


// ── ID3D11VideoProcessor: scale + BGRA→NV12 ──────────────────────────────────

struct VideoProcessorState {
    vp_device: ID3D11VideoDevice,
    vp_context: ID3D11VideoContext,
    vp_context1: Option<ID3D11VideoContext1>,
    vp_enum: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    src_width: u32,
    src_height: u32,
}

unsafe impl Send for VideoProcessorState {}
unsafe impl Sync for VideoProcessorState {}

impl VideoProcessorState {
    /// Create a video processor for BGRA→NV12 conversion + scaling.
    /// Pins source/dest rectangles explicitly (NVIDIA fix).
    unsafe fn new(
        device: &ID3D11Device,
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
        fps: u32,
    ) -> Result<Self> {
        let vp_device: ID3D11VideoDevice = device.cast()?;

        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            InputWidth: src_width,
            InputHeight: src_height,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            OutputWidth: dst_width,
            OutputHeight: dst_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };

        let vp_enum = vp_device.CreateVideoProcessorEnumerator(&content_desc)?;
        let vp = vp_device.CreateVideoProcessor(&vp_enum, 0)?;

        let context: ID3D11DeviceContext = device.GetImmediateContext()?;
        let vp_context: ID3D11VideoContext = context.cast()?;

        // Try to get VideoContext1 for color space methods (available on Win10+, graceful if absent)
        let vp_context1: Option<ID3D11VideoContext1> = context.cast().ok();

        // Pin source rectangle (NVIDIA fix: prevents auto-cropping)
        let src_rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: src_width as i32,
            bottom: src_height as i32,
        };
        vp_context.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&src_rect));

        // Pin destination rectangle
        let dst_rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: dst_width as i32,
            bottom: dst_height as i32,
        };
        vp_context.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&dst_rect));
        vp_context.VideoProcessorSetOutputTargetRect(&vp, true, Some(&dst_rect));

        Ok(Self {
            vp_device,
            vp_context,
            vp_context1,
            vp_enum,
            vp,
            src_width,
            src_height,
        })
    }

    /// Process one BGRA input texture → NV12 output texture.
    /// On HDR systems (detected by texture format), forces BT.709 SDR output.
    /// On SDR systems, leaves color handling to driver defaults (proven working).
    unsafe fn process(
        &self,
        input_tex: &ID3D11Texture2D,
        output_tex: &ID3D11Texture2D,
    ) -> Result<()> {
        // Only force color space conversion for actual HDR input formats.
        // SDR (BGRA8) is left alone — driver defaults produce correct colors.
        if let Some(ref vp_ctx1) = self.vp_context1 {
            let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
            input_tex.GetDesc(&mut tex_desc);
            let is_hdr_format = matches!(
                tex_desc.Format,
                DXGI_FORMAT_R16G16B16A16_FLOAT
                    | DXGI_FORMAT_R10G10B10A2_UNORM
                    | DXGI_FORMAT_R10G10B10_XR_BIAS_A2_UNORM
            );

            if is_hdr_format {
                // HDR input: force tonemap to BT.709 SDR output
                vp_ctx1.VideoProcessorSetStreamColorSpace1(&self.vp, 0,
                    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                vp_ctx1.VideoProcessorSetOutputColorSpace1(&self.vp,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            // SDR: do NOT set color space — let the driver use defaults
        }

        // Create input view (BGRA)
        let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        self.vp_device.CreateVideoProcessorInputView(
            input_tex,
            &self.vp_enum,
            &input_view_desc,
            Some(&mut input_view),
        )?;
        let input_view = input_view.context("VP input view")?;

        // Create output view (NV12)
        let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        self.vp_device.CreateVideoProcessorOutputView(
            output_tex,
            &self.vp_enum,
            &output_view_desc,
            Some(&mut output_view),
        )?;
        let output_view = output_view.context("VP output view")?;

        // Build stream data
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: ptr::null_mut(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
            ppFutureSurfaces: ptr::null_mut(),
            ..Default::default()
        }];

        self.vp_context.VideoProcessorBlt(&self.vp, &output_view, 0, &streams)?;

        // Drop COM references to prevent leak (60 objects/sec otherwise)
        std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);

        Ok(())
    }

    /// Update source dimensions (when capture target resizes).
    unsafe fn update_source_size(&mut self, device: &ID3D11Device, new_w: u32, new_h: u32, dst_w: u32, dst_h: u32, fps: u32) -> Result<()> {
        if new_w == self.src_width && new_h == self.src_height {
            return Ok(());
        }
        *self = Self::new(device, new_w, new_h, dst_w, dst_h, fps)?;
        Ok(())
    }
}

/// Create NV12 pool textures pre-filled with legal black (Y=16, U=V=128).
/// AMD fix: prevents green frame flash on first encode.
unsafe fn create_nv12_pool(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    width: u32,
    height: u32,
    count: usize,
) -> Result<Vec<ID3D11Texture2D>> {
    let mut pool = Vec::with_capacity(count);
    for _ in 0..count {
        // Try with BIND_RENDER_TARGET first (fastest path for VP output views on most GPUs).
        // Falls back to no bind flags if the driver rejects it (some NVIDIA drivers reject
        // BIND_RENDER_TARGET on NV12 at non-720p resolutions like 1080p).
        let desc_rt = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        let tex = match device.CreateTexture2D(&desc_rt, None, Some(&mut tex)) {
            Ok(()) => tex.context("NV12 pool texture")?,
            Err(_) => {
                // Fallback: no bind flags — VP output views still work via the video device path
                let desc_plain = D3D11_TEXTURE2D_DESC {
                    BindFlags: 0,
                    ..desc_rt
                };
                let mut tex2: Option<ID3D11Texture2D> = None;
                device.CreateTexture2D(&desc_plain, None, Some(&mut tex2))?;
                tex2.context("NV12 pool texture (fallback, no BIND_RENDER_TARGET)")?
            }
        };

        // Pre-fill with legal black via staging texture
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..desc_rt
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        let staging = staging.context("NV12 staging")?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_WRITE, 0, Some(&mut mapped))?;

        let pitch = mapped.RowPitch as usize;
        let p = mapped.pData as *mut u8;

        // Y plane: height rows, fill with 16 (legal black luma)
        for row in 0..height as usize {
            let row_ptr = p.add(row * pitch);
            std::ptr::write_bytes(row_ptr, 16u8, width as usize);
        }

        // UV plane: height/2 rows, fill with 128 (neutral chroma)
        let uv_offset = height as usize * pitch;
        for row in 0..(height / 2) as usize {
            let row_ptr = p.add(uv_offset + row * pitch);
            std::ptr::write_bytes(row_ptr, 128u8, width as usize);
        }

        context.Unmap(&staging, 0);
        context.CopyResource(&tex, &staging);

        pool.push(tex);
    }
    Ok(pool)
}


// ── Persistent H.264 Hardware Encoder (Async MFT) ─────────────────────────────

/// Initialize the hardware H.264 encoder following the NVIDIA-critical init order.
/// Returns (IMFTransform, IMFMediaEventGenerator).
unsafe fn init_hardware_encoder(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<(IMFTransform, IMFMediaEventGenerator)> {
    // 1. MFTEnumEx with HARDWARE | SORTANDFILTER
    let flags = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let in_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let out_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut activates_ptr: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count: u32 = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        flags,
        Some(&in_info),
        Some(&out_info),
        &mut activates_ptr,
        &mut count,
    )?;

    if count == 0 || activates_ptr.is_null() {
        anyhow::bail!("No hardware H.264 encoder found");
    }

    let activates_slice = std::slice::from_raw_parts(activates_ptr, count as usize);
    let activate = activates_slice[0]
        .as_ref()
        .context("First encoder activate is None")?;

    // 2. ActivateObject to get IMFTransform
    let transform: IMFTransform = activate.ActivateObject()?;

    // Release all IMFActivate COM objects before freeing the array.
    // Without this, entries [1..count] leak (never get Release() called).
    {
        let activates_owned = std::slice::from_raw_parts_mut(activates_ptr, count as usize);
        for slot in activates_owned.iter_mut() {
            let _ = slot.take(); // Drop calls Release()
        }
    }
    CoTaskMemFree(Some(activates_ptr as *const _));

    // 3. Unlock async: GetAttributes() -> SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, 1)
    let attrs = transform.GetAttributes()?;
    attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

    // 4. Create DXGI Device Manager (needed by hardware MFT for GPU-accelerated encoding)
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    let mut reset_token: u32 = 0;
    MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)?;
    let manager = manager.context("DXGI device manager")?;
    manager.ResetDevice(device, reset_token)?;

    // 5. SET_D3D_MANAGER before rate control and output type.
    let unk: windows::core::IUnknown = manager.cast()?;
    transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, unk.as_raw() as usize)?;

    // 6. ICodecAPI: rate control BEFORE SetOutputType (Clipsta Lite guardrail #5).
    //    AMD encoders return success for ICodecAPI changes after SetOutputType but
    //    silently ignore them. Setting CBR/VBV first ensures they take effect.
    if let Ok(codec_api) = transform.cast::<ICodecAPI>() {
        use windows::Win32::System::Variant::*;

        unsafe fn make_u32_variant(val: u32) -> VARIANT {
            let mut v = VARIANT::default();
            v.Anonymous.Anonymous = std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                Anonymous: VARIANT_0_0_0 { ulVal: val },
                ..Default::default()
            });
            v
        }

        unsafe fn make_bool_variant(val: bool) -> VARIANT {
            let mut v = VARIANT::default();
            v.Anonymous.Anonymous = std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                Anonymous: VARIANT_0_0_0 {
                    boolVal: windows::Win32::Foundation::VARIANT_BOOL(if val { -1i16 } else { 0i16 }),
                },
                ..Default::default()
            });
            v
        }

        // CBR rate control (mode 2)
        let val = make_u32_variant(2);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &val);

        // Target bitrate
        let val = make_u32_variant(bitrate_kbps * 1000);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &val);

        // VBV buffer size = 1 second of bitrate (guardrail #6: always configure alongside bitrate)
        let val = make_u32_variant(bitrate_kbps * 1000);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonBufferSize, &val);

        // Low latency mode
        let val = make_bool_variant(true);
        let _ = codec_api.SetValue(&CODECAPI_AVLowLatencyMode, &val);
    }

    // 7. SetOutputType (H.264, target resolution, target fps, High profile)
    //    AFTER rate control is configured (guardrail #5).
    let level: u32 = if height > 720 { 51 } else { 42 };
    let out_type: IMFMediaType = MFCreateMediaType()?;
    out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
    out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?; // High profile
    out_type.SetUINT32(&MF_MT_MPEG2_LEVEL, level)?;
    let _ = out_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, 1);
    let _ = out_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, 2);
    let _ = out_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, 2);
    let _ = out_type.SetUINT32(&MF_MT_YUV_MATRIX, 2);
    transform.SetOutputType(0, &out_type, 0)?;

    // 8. SetInputType (NV12, target resolution, target fps)
    let in_type: IMFMediaType = MFCreateMediaType()?;
    in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    // Input color space: limited range BT.709 (matches VP output)
    let _ = in_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, 1);
    let _ = in_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, 2);
    let _ = in_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, 2);
    let _ = in_type.SetUINT32(&MF_MT_YUV_MATRIX, 2);
    transform.SetInputType(0, &in_type, 0)?;

    // 8. ProcessMessage(NOTIFY_BEGIN_STREAMING), ProcessMessage(NOTIFY_START_OF_STREAM)
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

    // Get IMFMediaEventGenerator for blocking event loop
    let event_gen: IMFMediaEventGenerator = transform.cast()?;

    Ok((transform, event_gen))
}

/// Fallback encoder initialization with relaxed settings.
/// Uses Baseline profile (widest HW support), Level 4.0, VBR mode, no low-latency.
/// This bypasses driver bugs on newer GPUs that reject High profile or CBR in certain configs.
unsafe fn init_hardware_encoder_relaxed(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<(IMFTransform, IMFMediaEventGenerator)> {
    let flags = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let in_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let out_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut activates_ptr: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count: u32 = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        flags,
        Some(&in_info),
        Some(&out_info),
        &mut activates_ptr,
        &mut count,
    )?;

    if count == 0 || activates_ptr.is_null() {
        anyhow::bail!("No hardware H.264 encoder found (fallback)");
    }

    let activates_slice = std::slice::from_raw_parts(activates_ptr, count as usize);
    let activate = activates_slice[0]
        .as_ref()
        .context("First encoder activate is None (fallback)")?;

    let transform: IMFTransform = activate.ActivateObject()?;

    // Release all IMFActivate COM objects before freeing the array.
    {
        let activates_owned = std::slice::from_raw_parts_mut(activates_ptr, count as usize);
        for slot in activates_owned.iter_mut() {
            let _ = slot.take(); // Drop calls Release()
        }
    }
    CoTaskMemFree(Some(activates_ptr as *const _));

    // Unlock async
    let attrs = transform.GetAttributes()?;
    attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

    // DXGI Device Manager — must be set BEFORE SetOutputType for 1080p+ support
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    let mut reset_token: u32 = 0;
    MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)?;
    let manager = manager.context("DXGI device manager (fallback)")?;
    manager.ResetDevice(device, reset_token)?;

    let unk: windows::core::IUnknown = manager.cast()?;
    transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, unk.as_raw() as usize)?;

    // ICodecAPI: rate control BEFORE SetOutputType (Clipsta Lite guardrail #5).
    // This tier exists specifically to work around AMD driver quirks, so it must
    // follow the same ordering as the primary path — AMD encoders return success
    // for ICodecAPI changes made after SetOutputType but silently ignore them.
    if let Ok(codec_api) = transform.cast::<ICodecAPI>() {
        use windows::Win32::System::Variant::*;

        unsafe fn make_u32_variant(val: u32) -> VARIANT {
            let mut v = VARIANT::default();
            v.Anonymous.Anonymous = std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                Anonymous: VARIANT_0_0_0 { ulVal: val },
                ..Default::default()
            });
            v
        }

        // VBR rate control (0 = variable bitrate — widest driver support)
        let val = make_u32_variant(0);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &val);

        // Target bitrate
        let val = make_u32_variant(bitrate_kbps * 1000);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &val);

        // VBV buffer size = 2 seconds of bitrate (guardrail #6: always configure alongside bitrate).
        // Larger than the primary path (1s) because VBR benefits from a bigger buffer to smooth
        // rate oscillation, while still preventing unbounded I-frame spikes.
        let val = make_u32_variant(bitrate_kbps * 1000 * 2);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonBufferSize, &val);
    }

    // Output type: Baseline profile, adaptive level (maximum compatibility)
    // AFTER rate control is configured (guardrail #5).
    let level: u32 = if height > 720 { 51 } else { 40 };
    let out_type: IMFMediaType = MFCreateMediaType()?;
    out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
    out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, 66)?; // Baseline profile
    out_type.SetUINT32(&MF_MT_MPEG2_LEVEL, level)?;
    // Color space: limited range BT.709 (same as optimal path)
    let _ = out_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, 1);
    let _ = out_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, 2);
    let _ = out_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, 2);
    let _ = out_type.SetUINT32(&MF_MT_YUV_MATRIX, 2);
    transform.SetOutputType(0, &out_type, 0)?;

    // Input type
    let in_type: IMFMediaType = MFCreateMediaType()?;
    in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    // Input color space: limited range BT.709 (matches VP output)
    let _ = in_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, 1);
    let _ = in_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, 2);
    let _ = in_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, 2);
    let _ = in_type.SetUINT32(&MF_MT_YUV_MATRIX, 2);
    transform.SetInputType(0, &in_type, 0)?;

    // Start streaming
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

    let event_gen: IMFMediaEventGenerator = transform.cast()?;
    Ok((transform, event_gen))
}

/// Bare-minimum encoder initialization — last resort fallback.
/// Omits profile, level, rate control, and all optional parameters.
/// Only specifies the absolute minimum: frame size, frame rate, bitrate.
/// Lets the driver choose everything else (profile, level, rate control mode).
/// This should work on ANY GPU that has a hardware H.264 encoder.
unsafe fn init_hardware_encoder_bare(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
) -> Result<(IMFTransform, IMFMediaEventGenerator)> {
    let flags = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let in_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let out_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut activates_ptr: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count: u32 = 0;
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        flags,
        Some(&in_info),
        Some(&out_info),
        &mut activates_ptr,
        &mut count,
    )?;

    if count == 0 || activates_ptr.is_null() {
        anyhow::bail!("No hardware H.264 encoder found (bare)");
    }

    let activates_slice = std::slice::from_raw_parts(activates_ptr, count as usize);
    let activate = activates_slice[0]
        .as_ref()
        .context("First encoder activate is None (bare)")?;

    let transform: IMFTransform = activate.ActivateObject()?;

    {
        let activates_owned = std::slice::from_raw_parts_mut(activates_ptr, count as usize);
        for slot in activates_owned.iter_mut() {
            let _ = slot.take();
        }
    }
    CoTaskMemFree(Some(activates_ptr as *const _));

    // Unlock async
    let attrs = transform.GetAttributes()?;
    attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

    // DXGI Device Manager — set before output type
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    let mut reset_token: u32 = 0;
    MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)?;
    let manager = manager.context("DXGI device manager (bare)")?;
    manager.ResetDevice(device, reset_token)?;
    let unk: windows::core::IUnknown = manager.cast()?;
    transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, unk.as_raw() as usize)?;

    // Best-effort rate control — even the bare path should try to cap bitrate (guardrail #6).
    // If the driver rejects these, we proceed anyway (bare path is last-resort).
    if let Ok(codec_api) = transform.cast::<ICodecAPI>() {
        use windows::Win32::System::Variant::*;

        unsafe fn make_u32_variant_bare(val: u32) -> VARIANT {
            let mut v = VARIANT::default();
            v.Anonymous.Anonymous = std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                Anonymous: VARIANT_0_0_0 { ulVal: val },
                ..Default::default()
            });
            v
        }

        // VBR mode (most universally supported)
        let val = make_u32_variant_bare(0);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &val);

        // Target bitrate
        let val = make_u32_variant_bare(bitrate_kbps * 1000);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &val);

        // VBV buffer = 2 seconds (guardrail #6)
        let val = make_u32_variant_bare(bitrate_kbps * 1000 * 2);
        let _ = codec_api.SetValue(&CODECAPI_AVEncCommonBufferSize, &val);
    }

    // Output type: ONLY mandatory fields — no profile, no level
    let out_type: IMFMediaType = MFCreateMediaType()?;
    out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
    out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    // No profile, no level — let the driver pick the best it supports
    transform.SetOutputType(0, &out_type, 0)?;

    // Input type: NV12
    let in_type: IMFMediaType = MFCreateMediaType()?;
    in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    transform.SetInputType(0, &in_type, 0)?;

    // Start streaming
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

    let event_gen: IMFMediaEventGenerator = transform.cast()?;
    Ok((transform, event_gen))
}

// Send wrappers for COM types that cross thread boundaries.
// These are safe because the D3D11 device is created with multithread protection,
// and the MFT is used exclusively from the encoder thread after transfer.
struct SendTransform(IMFTransform);
unsafe impl Send for SendTransform {}

struct SendEventGen(IMFMediaEventGenerator);
unsafe impl Send for SendEventGen {}

struct SendTextures(Vec<ID3D11Texture2D>);
unsafe impl Send for SendTextures {}

/// Wrapper for AAC encoder MFT to allow Send across thread boundaries.
/// Safety: the encoder is only accessed from the audio callback thread
/// (serialized via parking_lot::Mutex).
struct SendAacEncoder(Option<IMFTransform>);
unsafe impl Send for SendAacEncoder {}
unsafe impl Sync for SendAacEncoder {}

/// Dedicated encoder thread function.
/// Owns the IMFTransform. Blocks on GetEvent to receive METransformNeedInput/METransformHaveOutput.
/// On NeedInput: receives FrameMsg from channel, creates DXGI surface buffer, calls ProcessInput.
/// On HaveOutput: calls ProcessOutput, extracts H.264 data, detects keyframes, pushes to ring.
fn encoder_thread_fn(
    transform: SendTransform,
    event_gen: SendEventGen,
    nv12_pool: SendTextures,
    rx: Receiver<FrameMsg>,
    ring: Arc<Mutex<EncodedMediaRing>>,
    stop: Arc<AtomicBool>,
    fps: u32,
    nv12_free_tx: SyncSender<usize>,
) {
    let transform = transform.0;
    let event_gen = event_gen.0;
    let nv12_pool = nv12_pool.0;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let log = |_msg: &str| {};  // Disabled for production

    log("encoder thread started, entering event loop");

    // Pre-allocate IMFSample + IMFMediaBuffer pool (one per NV12 texture).
    // Eliminates MFCreateDXGISurfaceBuffer + MFCreateSample calls at 60fps.
    // Each sample wraps its corresponding NV12 pool texture permanently;
    // we only update PTS/duration before each ProcessInput.
    let sample_pool: Vec<Option<IMFSample>> = unsafe {
        nv12_pool.iter().map(|tex| {
            let buffer = MFCreateDXGISurfaceBuffer(
                &ID3D11Texture2D::IID,
                tex,
                0,
                false,
            ).ok()?;
            let sample: IMFSample = MFCreateSample().ok()?;
            sample.AddBuffer(&buffer).ok()?;
            Some(sample)
        }).collect()
    };

    // Frame duplication tracking
    // Frame duplication tracking
    let mut last_texture_idx: usize = usize::MAX;
    let mut last_pts: i64 = 0;
    let mut last_duration: i64 = 10_000_000 / fps as i64;

    // NV12 free-list return: track which texture index was last submitted to ProcessInput.
    let mut in_flight_texture_idx: Option<usize> = None;

    let mut event_count: u64 = 0;

    loop {
        if stop.load(Ordering::Relaxed) {
            log("stop flag set, draining encoder");
            unsafe {
                let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
            }
            for _ in 0..200 {
                match unsafe { event_gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(event) => {
                        let et = unsafe { event.GetType().unwrap_or(0) };
                        if et == 602 {
                            if let Some(frame) = unsafe { extract_output(&transform, &ring) } {
                                ring.lock().push_video(frame);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(idx) = in_flight_texture_idx.take() {
                let _ = nv12_free_tx.try_send(idx);
            }
            log("encoder thread exiting");
            break;
        }

        // Block waiting for an event from the async MFT
        if event_count == 0 {
            log("calling first GetEvent (blocking)...");
        }
        let event = match unsafe { event_gen.GetEvent(MF_EVENT_FLAG_NONE) } {
            Ok(ev) => {
                if event_count < 5 || event_count % 120 == 0 {
                    let et = unsafe { ev.GetType().unwrap_or(0) };
                    log(&format!("event {} received: type={}", event_count, et));
                }
                event_count += 1;
                ev
            }
            Err(e) => {
                log(&format!("GetEvent failed: {}, exiting", e));
                break;
            }
        };

        let event_type = unsafe { event.GetType().unwrap_or(0) };

        match event_type {
            // METransformNeedInput (601)
            601 => {
                // Return the previous texture to the free pool
                if let Some(idx) = in_flight_texture_idx.take() {
                    let _ = nv12_free_tx.try_send(idx);
                }

                // Wait for a real frame from WGC
                let msg = match rx.recv() {
                    Ok(m) => {
                        last_texture_idx = m.texture_index;
                        last_pts = m.pts_100ns;
                        last_duration = m.duration_100ns;
                        m
                    }
                    Err(_) => {
                        log("channel disconnected, exiting");
                        break;
                    }
                };

                unsafe {
                    if let Some(Some(ref sample)) = sample_pool.get(msg.texture_index) {
                        let _ = sample.SetSampleTime(msg.pts_100ns);
                        let _ = sample.SetSampleDuration(msg.duration_100ns);

                        if let Err(e) = transform.ProcessInput(0, sample, 0) {
                            log(&format!("ProcessInput failed: {}", e));
                        }
                    } else {
                        // Fallback: pool entry creation failed at init
                        let tex = &nv12_pool[msg.texture_index];
                        match MFCreateDXGISurfaceBuffer(
                            &ID3D11Texture2D::IID,
                            tex,
                            0,
                            false,
                        ) {
                            Ok(buffer) => {
                                let sample: IMFSample = match MFCreateSample() {
                                    Ok(s) => s,
                                    Err(_) => continue,
                                };
                                let _ = sample.AddBuffer(&buffer);
                                let _ = sample.SetSampleTime(msg.pts_100ns);
                                let _ = sample.SetSampleDuration(msg.duration_100ns);

                                if let Err(e) = transform.ProcessInput(0, &sample, 0) {
                                    log(&format!("ProcessInput failed: {}", e));
                                }
                            }
                            Err(e) => {
                                log(&format!("MFCreateDXGISurfaceBuffer failed: {}", e));
                            }
                        }
                    }
                }
                in_flight_texture_idx = Some(msg.texture_index);
            }
            // METransformHaveOutput (602)
            602 => {
                if let Some(frame) = unsafe { extract_output(&transform, &ring) } {
                    let is_kf = frame.is_keyframe;
                    let data_len = frame.data.len();
                    ring.lock().push_video(frame);
                    if event_count < 20 || event_count % 120 == 0 {
                        log(&format!("encoded frame: {}B keyframe={}", data_len, is_kf));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract one encoded output from the MFT (called on METransformHaveOutput).
/// The async MFT provides its own output sample.
/// Uses the ring's video buffer pool to recycle allocations instead of heap-allocating per frame.
unsafe fn extract_output(transform: &IMFTransform, ring: &Arc<Mutex<EncodedMediaRing>>) -> Option<EncodedFrame> {
    let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: std::mem::ManuallyDrop::new(None), // MFT provides its own sample
        dwStatus: 0,
        pEvents: std::mem::ManuallyDrop::new(None),
    };

    let mut status: u32 = 0;
    let hr = transform.ProcessOutput(
        0,
        std::slice::from_mut(&mut output_buffer),
        &mut status,
    );

    if hr.is_err() {
        return None;
    }

    let sample = std::mem::ManuallyDrop::into_inner(output_buffer.pSample)?;

    let pts = sample.GetSampleTime().unwrap_or(0);
    let dur = sample.GetSampleDuration().unwrap_or(0);

    let buf_count = sample.GetBufferCount().unwrap_or(0);
    if buf_count == 0 {
        return None;
    }
    let buf = sample.GetBufferByIndex(0).ok()?;
    let mut p: *mut u8 = ptr::null_mut();
    let mut len: u32 = 0;
    buf.Lock(&mut p, None, Some(&mut len)).ok()?;

    let data = if len > 0 && !p.is_null() {
        // Acquire a recycled buffer from the pool (avoids heap alloc per frame).
        let mut data = ring.lock().acquire_video_buffer();
        let src = std::slice::from_raw_parts(p, len as usize);
        data.reserve(src.len().saturating_sub(data.capacity()));
        data.extend_from_slice(src);
        data
    } else {
        let _ = buf.Unlock();
        return None;
    };
    let _ = buf.Unlock();

    let is_keyframe = is_nalu_keyframe(&data);

    Some(EncodedFrame {
        data: Arc::new(data),
        pts_100ns: pts,
        duration_100ns: dur,
        is_keyframe,
    })
}

/// Detect keyframes by parsing NAL unit headers.
/// Look for IDR (type 5) or SPS (type 7).
fn is_nalu_keyframe(data: &[u8]) -> bool {
    if data.len() < 5 {
        return false;
    }
    let mut i = 0;
    while i < data.len().saturating_sub(4) {
        let is_4byte_start =
            data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1;
        let is_3byte_start = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;

        if is_4byte_start {
            let nal_type = data[i + 4] & 0x1F;
            if nal_type == 5 || nal_type == 7 {
                return true;
            }
            i += 4;
        } else if is_3byte_start {
            let nal_type = data[i + 3] & 0x1F;
            if nal_type == 5 || nal_type == 7 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}


// ── EncodedMediaRing: circular buffer of encoded H.264 + PCM audio ────────────

#[derive(Clone)]
struct EncodedFrame {
    /// Arc-wrapped frame data: cloning an EncodedFrame is now a cheap ref-count bump
    /// instead of deep-copying the entire H.264 NAL unit payload.
    data: Arc<Vec<u8>>,
    pts_100ns: i64,
    duration_100ns: i64,
    is_keyframe: bool,
}

#[derive(Clone)]
struct AudioChunk {
    /// Arc-wrapped audio samples: cloning is a cheap ref-count bump.
    data: Arc<Vec<f32>>,
    pts_100ns: i64,
    duration_100ns: i64,
}

/// Pre-encoded AAC audio chunk (encoded at capture time to avoid re-encoding on save).
#[derive(Clone)]
struct EncodedAudioChunk {
    /// AAC-encoded audio data.
    data: Arc<Vec<u8>>,
    pts_100ns: i64,
    duration_100ns: i64,
}

/// Pool of reusable Vec<u8> buffers to avoid per-frame heap allocation.
/// When a frame is pruned from the ring and its Arc refcount drops to 0,
/// the Vec is returned to the pool for reuse by the next encoder output.
struct FrameBufferPool {
    /// Available buffers ready for reuse (cleared but capacity preserved).
    free: Vec<Vec<u8>>,
    /// Maximum buffers to keep in the pool (prevents unbounded growth).
    max_pooled: usize,
}

impl FrameBufferPool {
    fn new(max_pooled: usize) -> Self {
        Self {
            free: Vec::with_capacity(max_pooled),
            max_pooled,
        }
    }

    /// Get a buffer from the pool (reuses existing capacity) or allocate a new one.
    #[allow(dead_code)]
    fn acquire(&mut self) -> Vec<u8> {
        self.free.pop().unwrap_or_else(|| Vec::with_capacity(4096))
    }

    /// Return a buffer to the pool for reuse. Clears content but keeps allocation.
    fn release(&mut self, mut buf: Vec<u8>) {
        if self.free.len() < self.max_pooled {
            buf.clear();
            self.free.push(buf);
        }
        // else: drop it (pool is full)
    }
}

/// Pool of reusable Vec<f32> buffers for audio chunks.
struct AudioBufferPool {
    free: Vec<Vec<f32>>,
    max_pooled: usize,
}

impl AudioBufferPool {
    fn new(max_pooled: usize) -> Self {
        Self {
            free: Vec::with_capacity(max_pooled),
            max_pooled,
        }
    }

    #[allow(dead_code)]
    fn acquire(&mut self) -> Vec<f32> {
        self.free.pop().unwrap_or_else(|| Vec::with_capacity(960))
    }

    fn release(&mut self, mut buf: Vec<f32>) {
        if self.free.len() < self.max_pooled {
            buf.clear();
            self.free.push(buf);
        }
    }
}

// ── Memory-Mapped Video Ring Buffer ───────────────────────────────────────────

/// Metadata for a single frame stored in the mmap ring.
#[derive(Clone, Copy)]
struct FrameEntry {
    /// Byte offset in the mmap file where this frame's data starts.
    offset: u64,
    /// Length of the H.264 frame data in bytes.
    len: u32,
    /// Presentation timestamp in 100ns units.
    pts_100ns: i64,
    /// Frame duration in 100ns units.
    duration_100ns: i64,
    /// Whether this frame is a keyframe (IDR/SPS).
    is_keyframe: bool,
}

/// Memory-mapped circular buffer for H.264 video frames.
/// Frame data lives on disk (memory-mapped), only metadata lives in RAM.
/// The OS pages in only the actively-accessed regions (~50MB) while the full
/// buffer can hold 5+ minutes of 1080p video without consuming heap memory.
struct MmapVideoRing {
    /// Memory-mapped file holding raw H.264 frame data.
    mmap: memmap2::MmapMut,
    /// Path to the backing file (for cleanup).
    file_path: std::path::PathBuf,
    /// Total capacity of the mmap in bytes.
    capacity: u64,
    /// Current write position (wraps around at capacity).
    write_cursor: u64,
    /// Frame metadata index (in-memory, ~32 bytes per frame).
    frames: VecDeque<FrameEntry>,
    /// Absolute frame count (total frames ever written).
    frames_pushed: usize,
    /// Indices into `frames` that are keyframes.
    keyframe_indices: VecDeque<usize>,
    /// Maximum buffer duration in 100ns units.
    max_duration_100ns: i64,
}

impl MmapVideoRing {
    /// Create a new memory-mapped video ring buffer.
    /// `max_seconds`: maximum duration to keep.
    /// `bitrate_kbps`: expected video bitrate (determines file size).
    fn new(max_seconds: u32, bitrate_kbps: u32) -> anyhow::Result<Self> {
        // Size the file: bitrate × duration × 1.3 (headroom for I-frame spikes)
        let bytes_per_sec = (bitrate_kbps as u64 * 1000) / 8;
        let capacity = bytes_per_sec * max_seconds as u64 * 13 / 10;
        // Minimum 100 MB, maximum 1500 MB (1.5 GB).
        // The ring wraps and prunes by duration, so exceeding this cap just means
        // the file wraps sooner (not data loss). 1.5 GB supports 5 min of
        // 1080p/60fps at 20 Mbps with headroom for AMD's 50% bitrate boost.
        let capacity = capacity.max(100 * 1024 * 1024).min(1500 * 1024 * 1024);

        let file_path = std::env::temp_dir().join("clipsta_ring_video.bin");
        // Remove old file if it exists
        let _ = std::fs::remove_file(&file_path);

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&file_path)?;
        file.set_len(capacity)?;

        // Memory-map the file
        let mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };

        Ok(Self {
            mmap,
            file_path,
            capacity,
            write_cursor: 0,
            frames: VecDeque::with_capacity(max_seconds as usize * 60),
            frames_pushed: 0,
            keyframe_indices: VecDeque::new(),
            max_duration_100ns: max_seconds as i64 * 10_000_000,
        })
    }

    /// Write a frame's H.264 data into the ring and record its metadata.
    fn push_frame(&mut self, data: &[u8], pts_100ns: i64, duration_100ns: i64, is_keyframe: bool) {
        let len = data.len() as u32;
        if len == 0 { return; }

        // A single frame larger than the whole ring can never be stored coherently.
        // Dropping it is preferable to corrupting the buffer (this should never happen
        // in practice — 1.5 GB capacity vs. worst-case single I-frame of a few MB).
        if len as u64 > self.capacity {
            eprintln!(
                "[gpu_capture] Dropping frame larger than ring capacity ({} B > {} B)",
                len, self.capacity
            );
            return;
        }

        // Check if we need to wrap around. If the frame won't fit in the remaining
        // tail space, restart at the beginning of the file.
        let end_pos = self.write_cursor + len as u64;
        if end_pos > self.capacity {
            // Wrap: write at the beginning.
            self.write_cursor = 0;
        }

        let start = self.write_cursor;
        let end = start + len as u64;

        // CRITICAL (corruption fix): evict metadata for any frame whose byte region
        // overlaps the region we are about to overwrite. Frames are written
        // contiguously in order, so overwritten frames are always the oldest ones
        // at the front of the deque. Previously `prune()` only dropped frames by
        // *duration*, so after a wrap the write cursor could overwrite the bytes of
        // frames that were still referenced in metadata — `read_frame` then returned
        // garbage and produced corrupted saved clips. We now drop those metadata
        // entries here so the ring never hands out a slice into overwritten bytes.
        self.evict_overlapping(start, end);

        // Write frame data to mmap
        let start_us = start as usize;
        let end_us = end as usize;
        self.mmap[start_us..end_us].copy_from_slice(data);

        // Record metadata
        let entry = FrameEntry {
            offset: start,
            len,
            pts_100ns,
            duration_100ns,
            is_keyframe,
        };

        if is_keyframe {
            self.keyframe_indices.push_back(self.frames_pushed);
        }
        self.frames.push_back(entry);
        self.frames_pushed += 1;
        self.write_cursor = end;

        // Prune old frames that exceed max duration
        self.prune();
    }

    /// Drop metadata for front frames whose byte range [offset, offset+len)
    /// intersects the write region [start, end). This keeps the metadata index
    /// consistent with the physical bytes after a wrap-around, preventing reads
    /// of overwritten data. Because frames are laid down in write order, any frame
    /// physically colliding with the new write is at the front of the deque.
    fn evict_overlapping(&mut self, start: u64, end: u64) {
        while let Some(front) = self.frames.front() {
            let f_start = front.offset;
            let f_end = front.offset + front.len as u64;
            // Overlap test for half-open intervals [start,end) vs [f_start,f_end).
            let overlaps = f_start < end && start < f_end;
            if overlaps {
                self.frames.pop_front();
                self.sync_keyframe_indices_after_pop();
            } else {
                break;
            }
        }
    }

    /// After popping the front frame, drop any keyframe indices that now point
    /// at or before the new base offset (i.e. frames no longer in the deque).
    fn sync_keyframe_indices_after_pop(&mut self) {
        let base = self.frames_pushed - self.frames.len();
        while let Some(&ki) = self.keyframe_indices.front() {
            if ki < base {
                self.keyframe_indices.pop_front();
            } else {
                break;
            }
        }
    }

    /// Remove frames that exceed the maximum buffer duration.
    fn prune(&mut self) {
        while self.frames.len() > 2 {
            let newest_pts = self.frames.back().map(|f| f.pts_100ns).unwrap_or(0);
            let oldest_pts = self.frames.front().map(|f| f.pts_100ns).unwrap_or(0);
            if newest_pts - oldest_pts > self.max_duration_100ns {
                self.frames.pop_front();
                self.sync_keyframe_indices_after_pop();
            } else {
                break;
            }
        }
    }

    /// Find the keyframe at or before (newest_pts - requested_seconds).
    fn find_slice_start(&self, seconds: u32) -> Option<usize> {
        if self.frames.is_empty() {
            return None;
        }
        let newest_pts = self.frames.back()?.pts_100ns;
        let target_pts = newest_pts - (seconds as i64 * 10_000_000);

        let base_offset = self.frames_pushed - self.frames.len();

        let mut best: Option<usize> = None;
        for &abs_idx in self.keyframe_indices.iter().rev() {
            let local_idx = abs_idx.saturating_sub(base_offset);
            if local_idx >= self.frames.len() {
                continue;
            }
            let frame = &self.frames[local_idx];
            if frame.pts_100ns <= target_pts {
                best = Some(local_idx);
                break;
            }
            best = Some(local_idx);
        }

        if best.is_none() {
            for &abs_idx in self.keyframe_indices.iter() {
                let local_idx = abs_idx.saturating_sub(base_offset);
                if local_idx < self.frames.len() {
                    best = Some(local_idx);
                    break;
                }
            }
        }

        best
    }

    /// Read a frame's data from the mmap. Returns a slice reference.
    fn read_frame(&self, entry: &FrameEntry) -> &[u8] {
        let start = entry.offset as usize;
        let end = start + entry.len as usize;
        &self.mmap[start..end]
    }

    /// Iterate frame entries from start_idx onward.
    fn iter_frames_from(&self, start_idx: usize) -> impl Iterator<Item = &FrameEntry> {
        self.frames.iter().skip(start_idx)
    }

    /// Get the PTS of the first frame in the buffer.
    fn oldest_pts(&self) -> i64 {
        self.frames.front().map(|f| f.pts_100ns).unwrap_or(0)
    }

    /// Get the PTS + duration of the last frame.
    fn newest_end_pts(&self) -> i64 {
        self.frames.back().map(|f| f.pts_100ns + f.duration_100ns).unwrap_or(0)
    }

    /// Duration of buffered content in seconds.
    fn duration_secs(&self) -> f64 {
        let newest = self.frames.back().map(|f| f.pts_100ns).unwrap_or(0);
        let oldest = self.frames.front().map(|f| f.pts_100ns).unwrap_or(0);
        (newest - oldest) as f64 / 10_000_000.0
    }
}

impl Drop for MmapVideoRing {
    fn drop(&mut self) {
        // Clean up the temp file
        let _ = std::fs::remove_file(&self.file_path);
    }
}

/// Ring buffer holding encoded H.264 frames + PCM audio chunks.
/// Maintains a keyframe index for fast slicing.
///
/// Memory optimization:
/// - Frame data is Arc-wrapped: slice_video is O(n) Arc clones, not deep copies
/// - Buffer pools recycle allocations: pruned frames' Vecs are reused by new frames
pub(crate) struct EncodedMediaRing {
    /// Memory-mapped video ring (primary path — frames on disk, metadata in RAM).
    video_mmap: Option<MmapVideoRing>,
    /// Fallback: in-memory video frames (used if mmap creation fails).
    video_frames: VecDeque<EncodedFrame>,
    audio_chunks: VecDeque<AudioChunk>,
    /// Pre-encoded AAC chunks (encoded at capture time for fast save)
    encoded_audio_chunks: VecDeque<EncodedAudioChunk>,
    /// Separate mic audio chunks (used when multi_track_audio is enabled)
    mic_audio_chunks: VecDeque<AudioChunk>,
    /// Indices into video_frames that are keyframes (for fast seeking)
    keyframe_indices: VecDeque<usize>,
    /// Running offset: total frames ever pushed (to convert absolute index → deque index)
    frames_pushed: usize,
    max_duration_100ns: i64,
    /// Pool for recycling video frame buffers
    video_pool: FrameBufferPool,
    /// Pool for recycling audio chunk buffers
    audio_pool: AudioBufferPool,
}

unsafe impl Send for EncodedMediaRing {}
unsafe impl Sync for EncodedMediaRing {}

impl EncodedMediaRing {
    fn new(max_seconds: u32) -> Self {
        Self::new_with_bitrate(max_seconds, 20000) // Default 20 Mbps estimate
    }

    fn new_with_bitrate(max_seconds: u32, bitrate_kbps: u32) -> Self {
        // Memory-mapped video ring: stores H.264 frames in a temp file instead of heap.
        // Previously disabled because WebView2's page fault pressure caused crashes during saves.
        // Now SAFE: capture runs in clipsta-capture.exe which has NO WebView2.
        let video_mmap = match MmapVideoRing::new(max_seconds, bitrate_kbps) {
            Ok(ring) => {
                eprintln!("[gpu_capture] Mmap ring enabled: ~{} MB file at {}",
                    ring.capacity / (1024 * 1024),
                    ring.file_path.display());
                Some(ring)
            }
            Err(e) => {
                eprintln!("[gpu_capture] Mmap ring failed (falling back to in-memory): {}", e);
                None
            }
        };
        let use_mmap = video_mmap.is_some();

        Self {
            video_mmap,
            video_frames: VecDeque::with_capacity(if !use_mmap { max_seconds as usize * 60 } else { 0 }),
            audio_chunks: VecDeque::with_capacity(max_seconds as usize * 50),
            encoded_audio_chunks: VecDeque::with_capacity(max_seconds as usize * 50),
            mic_audio_chunks: VecDeque::with_capacity(max_seconds as usize * 50),
            keyframe_indices: VecDeque::new(),
            frames_pushed: 0,
            max_duration_100ns: max_seconds as i64 * 10_000_000,
            video_pool: FrameBufferPool::new(256),
            audio_pool: AudioBufferPool::new(128),
        }
    }

    /// Get a video buffer from the pool (for use by extract_output).
    #[allow(dead_code)]
    fn acquire_video_buffer(&mut self) -> Vec<u8> {
        self.video_pool.acquire()
    }

    /// Get an audio buffer from the pool (for use by audio callback).
    #[allow(dead_code)]
    fn acquire_audio_buffer(&mut self, min_capacity: usize) -> Vec<f32> {
        let mut buf = self.audio_pool.acquire();
        if buf.capacity() < min_capacity {
            buf.reserve(min_capacity - buf.capacity());
        }
        buf
    }

    /// Push an encoded video frame into the ring.
    fn push_video(&mut self, frame: EncodedFrame) {
        if let Some(ref mut mmap_ring) = self.video_mmap {
            // Primary path: write to memory-mapped file (no heap allocation)
            mmap_ring.push_frame(&frame.data, frame.pts_100ns, frame.duration_100ns, frame.is_keyframe);
        } else {
            // Fallback: in-memory VecDeque (original behavior)
            if frame.is_keyframe {
                self.keyframe_indices.push_back(self.frames_pushed);
            }
            self.video_frames.push_back(frame);
            self.frames_pushed += 1;
            self.prune();
        }
    }

    /// Push a PCM audio chunk into the ring.
    fn push_audio(&mut self, chunk: AudioChunk) {
        self.audio_chunks.push_back(chunk);
        self.prune_audio();
    }

    /// Push a pre-encoded AAC audio chunk into the ring.
    fn push_encoded_audio(&mut self, chunk: EncodedAudioChunk) {
        self.encoded_audio_chunks.push_back(chunk);
        self.prune_encoded_audio();
    }

    /// Push a mic-only PCM audio chunk (for multi-track mode).
    fn push_mic_audio(&mut self, chunk: AudioChunk) {
        self.mic_audio_chunks.push_back(chunk);
        self.prune_mic_audio();
    }

    /// Remove old frames that exceed max buffer duration.
    /// Recycles buffer allocations back to the pool when Arc refcount is 1
    /// (meaning only the ring holds a reference — no active save_clip is using it).
    fn prune(&mut self) {
        while self.video_frames.len() > 2 {
            let newest_pts = self.video_frames.back().map(|f| f.pts_100ns).unwrap_or(0);
            let oldest_pts = self.video_frames.front().map(|f| f.pts_100ns).unwrap_or(0);
            if newest_pts - oldest_pts > self.max_duration_100ns {
                if let Some(old_frame) = self.video_frames.pop_front() {
                    // Recycle the buffer if we're the sole owner
                    if let Ok(buf) = Arc::try_unwrap(old_frame.data) {
                        self.video_pool.release(buf);
                    }
                }
                let base = self.frames_pushed - self.video_frames.len() - 1;
                while let Some(&ki) = self.keyframe_indices.front() {
                    if ki <= base {
                        self.keyframe_indices.pop_front();
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn prune_audio(&mut self) {
        let oldest_video_pts = if let Some(ref mmap_ring) = self.video_mmap {
            mmap_ring.oldest_pts()
        } else {
            if self.video_frames.is_empty() { return; }
            self.video_frames.front().map(|f| f.pts_100ns).unwrap_or(0)
        };
        if oldest_video_pts == 0 { return; }
        while let Some(front) = self.audio_chunks.front() {
            if front.pts_100ns + front.duration_100ns < oldest_video_pts {
                if let Some(old_chunk) = self.audio_chunks.pop_front() {
                    if let Ok(buf) = Arc::try_unwrap(old_chunk.data) {
                        self.audio_pool.release(buf);
                    }
                }
            } else {
                break;
            }
        }
    }

    fn prune_encoded_audio(&mut self) {
        let oldest_video_pts = if let Some(ref mmap_ring) = self.video_mmap {
            mmap_ring.oldest_pts()
        } else {
            if self.video_frames.is_empty() { return; }
            self.video_frames.front().map(|f| f.pts_100ns).unwrap_or(0)
        };
        if oldest_video_pts == 0 { return; }
        while let Some(front) = self.encoded_audio_chunks.front() {
            if front.pts_100ns + front.duration_100ns < oldest_video_pts {
                self.encoded_audio_chunks.pop_front();
            } else {
                break;
            }
        }
    }

    fn prune_mic_audio(&mut self) {
        let oldest_video_pts = if let Some(ref mmap_ring) = self.video_mmap {
            mmap_ring.oldest_pts()
        } else {
            if self.video_frames.is_empty() { return; }
            self.video_frames.front().map(|f| f.pts_100ns).unwrap_or(0)
        };
        if oldest_video_pts == 0 { return; }
        while let Some(front) = self.mic_audio_chunks.front() {
            if front.pts_100ns + front.duration_100ns < oldest_video_pts {
                if let Some(old_chunk) = self.mic_audio_chunks.pop_front() {
                    if let Ok(buf) = Arc::try_unwrap(old_chunk.data) {
                        self.audio_pool.release(buf);
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Find the keyframe at or before (newest_pts - requested_seconds).
    /// Returns the deque-local index of that keyframe.
    fn find_slice_start(&self, seconds: u32) -> Option<usize> {
        if let Some(ref mmap_ring) = self.video_mmap {
            return mmap_ring.find_slice_start(seconds);
        }
        // Fallback: in-memory path
        if self.video_frames.is_empty() {
            return None;
        }
        let newest_pts = self.video_frames.back()?.pts_100ns;
        let target_pts = newest_pts - (seconds as i64 * 10_000_000);

        let base_offset = self.frames_pushed - self.video_frames.len();

        let mut best: Option<usize> = None;
        for &abs_idx in self.keyframe_indices.iter().rev() {
            let local_idx = abs_idx.saturating_sub(base_offset);
            if local_idx >= self.video_frames.len() {
                continue;
            }
            let frame = &self.video_frames[local_idx];
            if frame.pts_100ns <= target_pts {
                best = Some(local_idx);
                break;
            }
            best = Some(local_idx);
        }

        if best.is_none() {
            for &abs_idx in self.keyframe_indices.iter() {
                let local_idx = abs_idx.saturating_sub(base_offset);
                if local_idx < self.video_frames.len() {
                    best = Some(local_idx);
                    break;
                }
            }
        }

        best
    }

    /// Slice video frames from start_idx to end.
    /// When using mmap, reads frame data from disk (OS pages it in automatically).
    /// For in-memory path, returns full frames with Arc-cloned data (instant).
    fn slice_video(&self, start_idx: usize) -> Vec<EncodedFrame> {
        if let Some(ref mmap_ring) = self.video_mmap {
            // Read frames from mmap — OS pages in the data on demand
            return mmap_ring.iter_frames_from(start_idx)
                .map(|entry| EncodedFrame {
                    data: Arc::new(mmap_ring.read_frame(entry).to_vec()),
                    pts_100ns: entry.pts_100ns,
                    duration_100ns: entry.duration_100ns,
                    is_keyframe: entry.is_keyframe,
                })
                .collect();
        }
        // Fallback: in-memory path (Arc clones)
        self.video_frames.iter().skip(start_idx).cloned().collect()
    }

    /// Get the mmap file path (for reading outside lock).
    #[allow(dead_code)]
    fn mmap_file_path(&self) -> Option<std::path::PathBuf> {
        self.video_mmap.as_ref().map(|r| r.file_path.clone())
    }

    /// Slice audio chunks that overlap the given PTS range.
    /// With Arc-wrapped data, this is an O(n) Arc::clone — no deep copy of audio samples.
    fn slice_audio(&self, start_pts: i64, end_pts: i64) -> Vec<AudioChunk> {
        self.audio_chunks
            .iter()
            .filter(|c| c.pts_100ns + c.duration_100ns > start_pts && c.pts_100ns < end_pts)
            .cloned()
            .collect()
    }

    /// Slice pre-encoded AAC chunks that overlap the given PTS range.
    fn slice_encoded_audio(&self, start_pts: i64, end_pts: i64) -> Vec<EncodedAudioChunk> {
        self.encoded_audio_chunks
            .iter()
            .filter(|c| c.pts_100ns + c.duration_100ns > start_pts && c.pts_100ns < end_pts)
            .cloned()
            .collect()
    }

    /// Slice mic-only audio chunks that overlap the given PTS range (multi-track mode).
    fn slice_mic_audio(&self, start_pts: i64, end_pts: i64) -> Vec<AudioChunk> {
        self.mic_audio_chunks
            .iter()
            .filter(|c| c.pts_100ns + c.duration_100ns > start_pts && c.pts_100ns < end_pts)
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    fn duration_secs(&self) -> f64 {
        let newest = self.video_frames.back().map(|f| f.pts_100ns).unwrap_or(0);
        let oldest = self.video_frames.front().map(|f| f.pts_100ns).unwrap_or(0);
        (newest - oldest) as f64 / 10_000_000.0
    }
}

/// Separate audio ring buffer — completely decoupled from the video EncodedMediaRing.
/// Uses pre-allocated buffer pool (Clipsta Lite approach): after the initial fill,
/// the audio callback NEVER allocates heap memory. Pruned buffers are recycled.
pub(crate) struct AudioRingBuffer {
    chunks: std::collections::VecDeque<AudioChunk>,
    max_duration_100ns: i64,
    /// Pool of reusable Vec<f32> buffers recycled from pruned chunks.
    pool: Vec<Vec<f32>>,
}

unsafe impl Send for AudioRingBuffer {}
unsafe impl Sync for AudioRingBuffer {}

impl AudioRingBuffer {
    pub fn new(max_seconds: u32) -> Self {
        Self {
            chunks: std::collections::VecDeque::with_capacity(max_seconds as usize * 50),
            max_duration_100ns: max_seconds as i64 * 10_000_000,
            pool: Vec::with_capacity(64),
        }
    }

    /// Get a buffer from the pool (reuses existing allocation) or create a new one.
    /// Called from the audio callback to avoid heap allocation in steady state.
    pub fn acquire_buffer(&mut self, min_capacity: usize) -> Vec<f32> {
        if let Some(mut buf) = self.pool.pop() {
            buf.clear();
            if buf.capacity() < min_capacity {
                buf.reserve(min_capacity - buf.capacity());
            }
            buf
        } else {
            Vec::with_capacity(min_capacity)
        }
    }

    pub fn push(&mut self, chunk: AudioChunk) {
        self.chunks.push_back(chunk);
        self.prune();
    }

    fn prune(&mut self) {
        if self.chunks.len() < 2 { return; }
        let newest_pts = self.chunks.back().map(|c| c.pts_100ns).unwrap_or(0);
        while let Some(front) = self.chunks.front() {
            if newest_pts - front.pts_100ns > self.max_duration_100ns {
                if let Some(old) = self.chunks.pop_front() {
                    // Recycle the buffer back to the pool if we're the sole owner
                    if let Ok(buf) = Arc::try_unwrap(old.data) {
                        if self.pool.len() < 64 {
                            self.pool.push(buf);
                        }
                    }
                }
            } else {
                break;
            }
        }
    }

    pub fn slice(&self, start_pts: i64, end_pts: i64) -> Vec<AudioChunk> {
        self.chunks.iter()
            .filter(|c| c.pts_100ns + c.duration_100ns > start_pts && c.pts_100ns < end_pts)
            .cloned()
            .collect()
    }
}


// ── MP4 Muxer: MF Sink Writer for save operation ──────────────────────────────

/// Mux sliced H.264 frames + PCM audio → MP4 file using MF Sink Writer.
/// Video is passthrough (no re-encoding). Audio is AAC-encoded at mux time.
/// If mic_chunks is Some, writes a second audio track for mic (multi-track mode).
unsafe fn mux_to_mp4_ex(
    output_path: &str,
    video_frames: &[EncodedFrame],
    audio_chunks: &[AudioChunk],
    mic_chunks: Option<&[AudioChunk]>,
    fps: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    if video_frames.is_empty() {
        anyhow::bail!("No video frames to mux");
    }

    // Ensure COM is initialized on this thread (save runs on Tauri async thread).
    // MFStartup is process-wide (ref-counted) — the capture thread already called it.
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

    let mut attr: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attr, 2)?;
    let attr = attr.context("mux attributes")?;
    attr.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)?;
    attr.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;

    let path: HSTRING = output_path.into();
    let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&path, None, &attr)?;

    // Video stream: passthrough H.264 (already encoded — no re-encoding)
    // Do NOT hardcode profile/level — they must match the actual H.264 bitstream.
    // The SinkWriter infers these from the SPS NAL unit in the stream.
    let vout: IMFMediaType = MFCreateMediaType()?;
    vout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    vout.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    vout.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    vout.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    vout.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    vout.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    vout.SetUINT32(&MF_MT_AVG_BITRATE, if height > 720 { 20_000_000 } else { 10_000_000 })?;
    let video_stream = writer.AddStream(&vout)?;

    // For H.264 passthrough: no SetInputMediaType needed.
    // Just AddStream with the output type and WriteSample with raw H.264 data.
    // The SinkWriter muxes the pre-encoded bitstream directly into MP4.

    // Audio stream: PCM input → AAC output (MF handles encoding)
    let has_audio = !audio_chunks.is_empty();
    let audio_stream: Option<u32> = if has_audio {
        // Try to set up AAC audio. If it fails (bitrate config issue), save video-only.
        (|| -> Option<u32> {
            let aout: IMFMediaType = MFCreateMediaType().ok()?;
            aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
            aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000).ok()?; // 192 kbps AAC (matches ShadowPlay)
            aout.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1).ok()?;
            let _ = aout.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29);
            let idx = writer.AddStream(&aout).ok()?;

            let ain: IMFMediaType = MFCreateMediaType().ok()?;
            ain.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
            ain.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ok()?;
            ain.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE).ok()?;
            ain.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
            ain.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
            ain.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGN).ok()?;
            ain.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_SAMPLE_RATE * AUDIO_BLOCK_ALIGN).ok()?;
            writer.SetInputMediaType(idx, &ain, None).ok()?;
            Some(idx)
        })()
    } else {
        None
    };

    // Mic audio stream (track 2): PCM input → AAC output (multi-track mode only)
    let mic_stream: Option<u32> = if let Some(mic_data) = mic_chunks {
        if !mic_data.is_empty() {
            (|| -> Option<u32> {
                let aout: IMFMediaType = MFCreateMediaType().ok()?;
                aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
                aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ok()?;
                aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
                aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
                aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ok()?;
                aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000).ok()?;
                aout.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1).ok()?;
                let _ = aout.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29);
                let idx = writer.AddStream(&aout).ok()?;

                let ain: IMFMediaType = MFCreateMediaType().ok()?;
                ain.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
                ain.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ok()?;
                ain.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE).ok()?;
                ain.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
                ain.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
                ain.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGN).ok()?;
                ain.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_SAMPLE_RATE * AUDIO_BLOCK_ALIGN).ok()?;
                writer.SetInputMediaType(idx, &ain, None).ok()?;
                Some(idx)
            })()
        } else {
            None
        }
    } else {
        None
    };

    writer.BeginWriting()?;

    // Rebase PTS so clip starts at 0.
    // Use wall-clock PTS (from session_start.elapsed()) for BOTH video and audio.
    // This ensures:
    // 1. A/V sync: both tracks share the same time reference
    // 2. Correct duration: if 60s of wall-clock time was captured, clip is 60s
    //    regardless of actual frame count (frame pacing may drop some frames)
    // 3. Natural frame timing: slight jitter in frame delivery is preserved
    //    (invisible to viewer, but prevents duration truncation)
    let base_pts = video_frames[0].pts_100ns;

    // Write video frames with wall-clock PTS rebased to 0
    for frame in video_frames {
        let buf: IMFMediaBuffer = MFCreateMemoryBuffer(frame.data.len() as u32)?;
        let mut p: *mut u8 = ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        ptr::copy_nonoverlapping(frame.data.as_ptr(), p, frame.data.len());
        buf.Unlock()?;
        buf.SetCurrentLength(frame.data.len() as u32)?;

        let sample: IMFSample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime(frame.pts_100ns - base_pts)?;
        sample.SetSampleDuration(frame.duration_100ns)?;

        if frame.is_keyframe {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }

        writer.WriteSample(video_stream, &sample)?;
    }

    // Write audio chunks with wall-clock PTS rebased to 0 (same clock as video)
    if let Some(audio_idx) = audio_stream {
        for chunk in audio_chunks {
            let i16_buf: Vec<i16> = chunk
                .data
                .iter()
                .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            let byte_len = (i16_buf.len() * 2) as u32;
            let buf: IMFMediaBuffer = MFCreateMemoryBuffer(byte_len)?;
            let mut p: *mut u8 = ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            ptr::copy_nonoverlapping(i16_buf.as_ptr() as *const u8, p, byte_len as usize);
            buf.Unlock()?;
            buf.SetCurrentLength(byte_len)?;

            let sample: IMFSample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime((chunk.pts_100ns - base_pts).max(0))?;
            sample.SetSampleDuration(chunk.duration_100ns)?;
            writer.WriteSample(audio_idx, &sample)?;
        }
    }

    // Write mic audio track (multi-track mode)
    if let (Some(mic_idx), Some(mic_data)) = (mic_stream, mic_chunks) {
        for chunk in mic_data {
            let i16_buf: Vec<i16> = chunk
                .data
                .iter()
                .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            let byte_len = (i16_buf.len() * 2) as u32;
            let buf: IMFMediaBuffer = MFCreateMemoryBuffer(byte_len)?;
            let mut p: *mut u8 = ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            ptr::copy_nonoverlapping(i16_buf.as_ptr() as *const u8, p, byte_len as usize);
            buf.Unlock()?;
            buf.SetCurrentLength(byte_len)?;

            let sample: IMFSample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime((chunk.pts_100ns - base_pts).max(0))?;
            sample.SetSampleDuration(chunk.duration_100ns)?;
            writer.WriteSample(mic_idx, &sample)?;
        }
    }

    writer.Finalize()?;
    // Do NOT call MFShutdown here — MF is ref-counted process-wide and the capture
    // thread still needs it. MFShutdown is called when capture ends in run_gpu_capture.
    Ok(())
}

/// Mux sliced H.264 frames + pre-encoded AAC → MP4 file using MF Sink Writer.
/// Both video and audio are passthrough (no re-encoding). This is the fast path
/// when AAC was encoded at capture time.
unsafe fn mux_to_mp4_aac_passthrough(
    output_path: &str,
    video_frames: &[EncodedFrame],
    encoded_audio: &[EncodedAudioChunk],
    fps: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    if video_frames.is_empty() {
        anyhow::bail!("No video frames to mux");
    }

    // Ensure COM is initialized on this thread. MF is already started process-wide by capture thread.
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

    let mut attr: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attr, 2)?;
    let attr = attr.context("mux attributes")?;
    attr.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)?;
    attr.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;

    let path: HSTRING = output_path.into();
    let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&path, None, &attr)?;

    // Video stream: passthrough H.264 (already encoded — SinkWriter muxes directly)
    // Do NOT specify profile/level — the SinkWriter reads them from the H.264 SPS NAL.
    // Hardcoding profile/level here would crash if the encoder fallback used a different
    // profile than expected (e.g., Baseline instead of High).
    let vout: IMFMediaType = MFCreateMediaType()?;
    vout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    vout.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    vout.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
    vout.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
    vout.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    vout.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
    vout.SetUINT32(&MF_MT_AVG_BITRATE, if height > 720 { 20_000_000 } else { 10_000_000 })?;
    let video_stream = writer.AddStream(&vout)?;

    // Audio stream: passthrough AAC (already encoded at capture time)
    let has_audio = !encoded_audio.is_empty();
    let audio_stream: Option<u32> = if has_audio {
        (|| -> Option<u32> {
            let aout: IMFMediaType = MFCreateMediaType().ok()?;
            aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
            aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000).ok()?;
            aout.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1).ok()?;
            let _ = aout.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29);
            let idx = writer.AddStream(&aout).ok()?;
            // For AAC passthrough: do NOT call SetInputMediaType — just AddStream + WriteSample.
            // The SinkWriter accepts raw AAC frames directly when only the output type is set.
            Some(idx)
        })()
    } else {
        None
    };

    writer.BeginWriting()?;

    let base_pts = video_frames[0].pts_100ns;

    // Write video frames
    for frame in video_frames {
        let buf: IMFMediaBuffer = MFCreateMemoryBuffer(frame.data.len() as u32)?;
        let mut p: *mut u8 = ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        ptr::copy_nonoverlapping(frame.data.as_ptr(), p, frame.data.len());
        buf.Unlock()?;
        buf.SetCurrentLength(frame.data.len() as u32)?;

        let sample: IMFSample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime(frame.pts_100ns - base_pts)?;
        sample.SetSampleDuration(frame.duration_100ns)?;

        if frame.is_keyframe {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }

        writer.WriteSample(video_stream, &sample)?;
    }

    // Write pre-encoded AAC audio (passthrough — no encoding needed)
    if let Some(audio_idx) = audio_stream {
        for chunk in encoded_audio {
            let buf: IMFMediaBuffer = MFCreateMemoryBuffer(chunk.data.len() as u32)?;
            let mut p: *mut u8 = ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            ptr::copy_nonoverlapping(chunk.data.as_ptr(), p, chunk.data.len());
            buf.Unlock()?;
            buf.SetCurrentLength(chunk.data.len() as u32)?;

            let sample: IMFSample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime((chunk.pts_100ns - base_pts).max(0))?;
            sample.SetSampleDuration(chunk.duration_100ns)?;
            writer.WriteSample(audio_idx, &sample)?;
        }
    }

    writer.Finalize()?;
    // Do NOT call MFShutdown — capture thread still needs MF active.
    Ok(())
}


// ── Public API: CaptureSession ────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct CompletedSegment {
    pub path: String,
    pub index: u32,
    pub start_pts: f64,
    pub end_pts: f64,
    pub duration: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureReadyInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub segment_dir: String,
}

#[derive(Clone)]
pub struct CaptureOptions {
    pub source_id: Option<String>,
    pub fps: u32,
    pub no_audio: bool,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub target_width: Option<u32>,
    pub target_height: Option<u32>,
    pub bitrate_kbps: u32,
    pub segment_duration: u32,
    pub buffer_duration: u32,
    pub segment_dir: PathBuf,
    /// When true, mic and desktop audio are kept as separate tracks in the MP4.
    pub multi_track_audio: bool,
    /// Pre-warmed D3D11 device (consumed on first start, None afterwards).
    pub warm_cache: Option<Arc<Mutex<Option<WarmCache>>>>,
}

/// Pre-warmed GPU resources created at app launch for fast recording start.
/// Stored in an Option — consumed on first `start()` call, None afterwards.
pub struct WarmCache {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub winrt_device: IDirect3DDevice,
    pub gpu_vendor_id: u32,
}
unsafe impl Send for WarmCache {}
unsafe impl Sync for WarmCache {}

pub struct CaptureSession {
    pub is_recording: Arc<AtomicBool>,
    pub is_saving: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    pub(crate) saved_clips: Arc<Mutex<Vec<CompletedSegment>>>,
    segment_dir: Arc<Mutex<Option<PathBuf>>>,
    recording_start: Arc<Mutex<Option<std::time::Instant>>>,
    audio_file: Arc<Mutex<Option<String>>>,
    pub(crate) ring: Arc<Mutex<EncodedMediaRing>>,
    pub(crate) audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    /// Separate mic-only audio buffer, populated only when multi_track_audio is on.
    pub(crate) mic_audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    pub(crate) session_fps: Arc<AtomicU32>,
    pub(crate) session_width: Arc<AtomicU32>,
    pub(crate) session_height: Arc<AtomicU32>,
    pub(crate) clip_counter: Arc<AtomicU32>,
    /// Count of frames dropped due to encoder backpressure (try_send failed).
    /// Reset on each recording start. Exposed in diagnostics for debugging.
    pub frame_drops: Arc<AtomicU32>,
    /// Whether multi-track audio is enabled for this session.
    pub(crate) multi_track_audio: Arc<AtomicBool>,
    /// Handle to the capture thread for proper join on restart.
    capture_thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Pre-warmed D3D11 device + context created at app launch (saves ~100-200ms on first start).
    pub warm_cache: Arc<Mutex<Option<WarmCache>>>,
}

impl Default for CaptureSession {
    fn default() -> Self {
        Self {
            is_recording: Arc::new(AtomicBool::new(false)),
            is_saving: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            saved_clips: Arc::new(Mutex::new(Vec::new())),
            segment_dir: Arc::new(Mutex::new(None)),
            recording_start: Arc::new(Mutex::new(None)),
            audio_file: Arc::new(Mutex::new(None)),
            ring: Arc::new(Mutex::new(EncodedMediaRing::new(MAX_RING_SECONDS))),
            audio_buffer: Arc::new(Mutex::new(AudioRingBuffer::new(MAX_RING_SECONDS))),
            mic_audio_buffer: Arc::new(Mutex::new(AudioRingBuffer::new(MAX_RING_SECONDS))),
            session_fps: Arc::new(AtomicU32::new(60)),
            session_width: Arc::new(AtomicU32::new(OUTPUT_WIDTH)),
            session_height: Arc::new(AtomicU32::new(OUTPUT_HEIGHT)),
            clip_counter: Arc::new(AtomicU32::new(0)),
            frame_drops: Arc::new(AtomicU32::new(0)),
            multi_track_audio: Arc::new(AtomicBool::new(false)),
            capture_thread: Mutex::new(None),
            warm_cache: Arc::new(Mutex::new(None)),
        }
    }
}

impl CaptureSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-warm GPU resources in the background for fast recording start.
    /// Call this once at app launch. The warmed D3D11 device + context will be
    /// consumed by the first `start()` call, saving ~100-200ms of initialization.
    pub fn warm_start(&self) {
        let cache = self.warm_cache.clone();
        thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                // Initialize Media Foundation process-wide (ref-counted, safe to call multiple times)
                let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            }
            // Create D3D11 device on the primary adapter
            let result = unsafe {
                let hmon = MonitorFromPoint(
                    windows::Win32::Foundation::POINT { x: 0, y: 0 },
                    MONITOR_DEFAULTTOPRIMARY,
                );
                let adapter = find_adapter_for_monitor(hmon);
                create_d3d11_device(adapter.as_ref())
            };
            match result {
                Ok((device, context, winrt_device)) => {
                    // Detect GPU vendor
                    let gpu_vendor_id: u32 = unsafe {
                        let dxgi_device: Result<IDXGIDevice, _> = device.cast();
                        dxgi_device.ok()
                            .and_then(|d| d.GetAdapter().ok())
                            .and_then(|a| {
                                let a1: Result<IDXGIAdapter1, _> = a.cast();
                                a1.ok()
                            })
                            .and_then(|a| a.GetDesc1().ok())
                            .map(|desc| desc.VendorId)
                            .unwrap_or(0)
                    };
                    *cache.lock() = Some(WarmCache {
                        device,
                        context,
                        winrt_device,
                        gpu_vendor_id,
                    });
                    eprintln!("[gpu_capture] Warm-start complete: D3D11 device ready (vendor: 0x{:04X})", gpu_vendor_id);
                }
                Err(e) => {
                    eprintln!("[gpu_capture] Warm-start failed (non-fatal): {}", e);
                    // Not fatal — run_gpu_capture will create its own device as fallback
                }
            }
        });
    }

    pub fn start(
        &self,
        opts: CaptureOptions,
        _on_segment: Box<dyn Fn(CompletedSegment) + Send + 'static>,
        on_died: Option<Box<dyn FnOnce(String) + Send + 'static>>,
    ) -> Result<CaptureReadyInfo> {
        if self.is_recording.load(Ordering::Relaxed) {
            anyhow::bail!("Already recording");
        }
        if self.is_saving.load(Ordering::Relaxed) {
            anyhow::bail!("Save in progress");
        }

        // Wait for any previous capture thread to fully exit before starting a new one.
        // This prevents resource conflicts (encoder sessions, MFStartup/MFShutdown races)
        // that cause crashes specifically at 1080p where GPU resources are more constrained.
        // Use a bounded wait to avoid blocking the Tauri IPC thread indefinitely.
        if let Some(prev_thread) = self.capture_thread.lock().take() {
            // Signal stop to the previous thread (in case stop() wasn't called yet)
            self.stop_flag.store(true, Ordering::SeqCst);
            // Wait up to 3 seconds for it to finish. If it doesn't, abandon it
            // (the thread will clean up on its own eventually).
            let start = std::time::Instant::now();
            loop {
                if prev_thread.is_finished() {
                    let _ = prev_thread.join();
                    break;
                }
                if start.elapsed() > std::time::Duration::from_secs(3) {
                    eprintln!("[gpu_capture] WARNING: Previous capture thread didn't exit in 3s, proceeding anyway");
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        self.stop_flag.store(false, Ordering::SeqCst);
        *self.saved_clips.lock() = Vec::new();
        *self.segment_dir.lock() = Some(opts.segment_dir.clone());
        self.session_fps.store(opts.fps, Ordering::SeqCst);
        self.multi_track_audio.store(opts.multi_track_audio, Ordering::SeqCst);
        // Width/height are provisional here; run_gpu_capture will update them
        // once the actual capture dimensions are known (important for "native" resolution).
        self.session_width.store(opts.target_width.unwrap_or(OUTPUT_WIDTH), Ordering::SeqCst);
        self.session_height.store(opts.target_height.unwrap_or(OUTPUT_HEIGHT), Ordering::SeqCst);
        self.frame_drops.store(0, Ordering::SeqCst);

        // Reset ring buffer
        *self.ring.lock() = EncodedMediaRing::new_with_bitrate(opts.buffer_duration.min(MAX_RING_SECONDS), opts.bitrate_kbps);
        *self.audio_buffer.lock() = AudioRingBuffer::new(opts.buffer_duration.min(MAX_RING_SECONDS));
        *self.mic_audio_buffer.lock() = AudioRingBuffer::new(opts.buffer_duration.min(MAX_RING_SECONDS));

        let stop = self.stop_flag.clone();
        let is_recording = self.is_recording.clone();
        let ring = self.ring.clone();
        let audio_buffer = self.audio_buffer.clone();
        let mic_audio_buffer = self.mic_audio_buffer.clone();
        let frame_drops = self.frame_drops.clone();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<CaptureReadyInfo>>();

        let handle = thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let result = run_gpu_capture(opts, stop.clone(), ring, audio_buffer, mic_audio_buffer, ready_tx.clone(), frame_drops);
            let error_msg = match &result {
                Ok(()) => None,
                Err(e) => {
                    eprintln!("[gpu_capture] pipeline error: {}", e);
                    let _ = ready_tx.send(Err(anyhow::anyhow!("{}", e)));
                    Some(format!("{}", e))
                }
            };
            // If stop was NOT requested by user, this is an unexpected death — notify frontend
            let user_stopped = stop.load(Ordering::SeqCst);
            is_recording.store(false, Ordering::SeqCst);
            if !user_stopped {
                if let Some(cb) = on_died {
                    let reason = error_msg.unwrap_or_else(|| "Capture session ended unexpectedly".to_string());
                    cb(reason);
                }
            }
        });
        *self.capture_thread.lock() = Some(handle);

        let ready = ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| anyhow::anyhow!("Capture start timeout (10s)"))?;

        let info = ready?;
        // Update session dimensions with actual resolved values from the capture thread.
        // This is essential for "native" resolution where dimensions aren't known until capture starts.
        self.session_width.store(info.width, Ordering::SeqCst);
        self.session_height.store(info.height, Ordering::SeqCst);
        self.is_recording.store(true, Ordering::SeqCst);
        *self.recording_start.lock() = Some(std::time::Instant::now());
        *self.audio_file.lock() = None;
        Ok(info)
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.is_recording.store(false, Ordering::SeqCst);
    }

    pub fn get_segments(&self) -> Vec<CompletedSegment> {
        self.saved_clips.lock().clone()
    }

    pub fn get_segment_dir(&self) -> Option<PathBuf> {
        self.segment_dir.lock().clone()
    }

    pub fn cleanup(&self) {
        if let Some(dir) = self.segment_dir.lock().take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        *self.saved_clips.lock() = Vec::new();
        *self.ring.lock() = EncodedMediaRing::new(MAX_RING_SECONDS);
    }

    pub fn get_audio_file(&self) -> Option<String> {
        self.audio_file.lock().clone()
    }

    pub fn elapsed_secs(&self) -> Option<f64> {
        self.recording_start.lock().map(|start| start.elapsed().as_secs_f64())
    }

    pub fn finalize_pending_segments(&self) {
        // Ring-buffer architecture: nothing to finalize
    }

    /// Save a clip: slice from ring at keyframe boundary, mux to MP4.
    pub fn save_clip(&self, seconds: u32, output_path: &str) -> Result<String> {
        let log = |_msg: &str| {};  // Disabled for production
        log(&format!("save_clip called: {}s -> {}", seconds, output_path));

        if !self.is_recording.load(Ordering::Relaxed) {
            log("ERROR: not recording");
            anyhow::bail!("Not recording — cannot save clip");
        }

        // Prevent concurrent saves (hotkey double-press, etc)
        if self.is_saving.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
            log("ERROR: save already in progress");
            anyhow::bail!("Save already in progress");
        }
        // Ensure is_saving is reset on exit (success or error)
        struct SavingGuard<'a>(&'a AtomicBool);
        impl<'a> Drop for SavingGuard<'a> {
            fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); }
        }
        let _guard = SavingGuard(&self.is_saving);

        let fps = self.session_fps.load(Ordering::Relaxed);
        let width = self.session_width.load(Ordering::Relaxed);
        let height = self.session_height.load(Ordering::Relaxed);

        // Snapshot the ring with minimal lock hold time.
        // Split into two brief lock acquisitions so the encoder thread can push_video
        // between them — prevents the ~100-300ms stall that caused game hitches on save.
        // Read the real multi-track setting for this session. When on, we slice the
        // separate mic buffer and write it as a second MP4 audio track. Default off
        // keeps the single mixed track (mic mixed into desktop audio).
        let multi_track = self.multi_track_audio.load(Ordering::Relaxed);
        let (video_frames, audio_chunks, mic_audio_chunks) = {
            // Phase 1: grab video frames (brief lock — Arc clones are cheap refcount bumps)
            let (video, start_pts, end_pts) = {
                let ring = self.ring.lock();
                let start_idx = ring
                    .find_slice_start(seconds)
                    .ok_or_else(|| anyhow::anyhow!("No keyframe found in ring buffer"))?;
                let video = ring.slice_video(start_idx);
                if video.is_empty() {
                    anyhow::bail!("No video frames available for clip");
                }
                let s_pts = video[0].pts_100ns;
                let e_pts = video.last().map(|f| f.pts_100ns + f.duration_100ns).unwrap_or(s_pts);
                (video, s_pts, e_pts)
            };
            // Lock released here — encoder can push frames while we grab audio

            // Phase 2: grab audio from separate audio buffer (zero contention with encoder)
            let audio = {
                let abuf = self.audio_buffer.lock();
                abuf.slice(start_pts, end_pts)
            };

            // Phase 3: grab mic-only audio (multi-track mode only) from the separate buffer.
            let mic_audio: Vec<AudioChunk> = if multi_track {
                let mbuf = self.mic_audio_buffer.lock();
                mbuf.slice(start_pts, end_pts)
            } else {
                Vec::new()
            };

            (video, audio, mic_audio)
        };

        eprintln!("[gpu_capture] save_clip: {} video frames, {} audio chunks, {}x{} @ {}fps → {}",
            video_frames.len(), audio_chunks.len(), width, height, fps, output_path);
        log(&format!("ring slice: {} video frames, {} audio chunks, {} mic chunks",
            video_frames.len(), audio_chunks.len(), mic_audio_chunks.len()));

        // Mux to MP4: prefer pre-encoded AAC (fast passthrough) over PCM (requires re-encoding)
        // Each mux function calls MFStartup/MFShutdown internally (save thread != capture thread).
        log("calling mux_to_mp4...");
        let mic_for_mux = if multi_track && !mic_audio_chunks.is_empty() {
            Some(&mic_audio_chunks[..])
        } else {
            None
        };
        // Wrap mux in catch_unwind — a panic in MF Sink Writer must NOT crash the app.
        // This can happen if the encoder produced a stream the SinkWriter doesn't expect.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Always use PCM → AAC encoding at mux time (proven reliable path).
            // AAC passthrough was causing crashes on some configurations because the
            // real-time AAC encoder output format doesn't always match what the SinkWriter expects.
            log("using PCM audio (AAC encoding at mux time)");
            unsafe { mux_to_mp4_ex(output_path, &video_frames, &audio_chunks, mic_for_mux, fps, width, height) }
        }));
        let result = match result {
            Ok(inner) => inner,
            Err(_) => Err(anyhow::anyhow!("Mux crashed unexpectedly — try again or switch to 720p")),
        };
        match &result {
            Ok(()) => eprintln!("[gpu_capture] save_clip: mux OK → {}", output_path),
            Err(e) => {
                eprintln!("[gpu_capture] save_clip: mux FAILED: {}", e);
                // Clean up partial/corrupt MP4 file on failure
                let _ = std::fs::remove_file(output_path);
            }
        }
        result?;

        // Track the saved clip
        let clip_idx = self.clip_counter.fetch_add(1, Ordering::Relaxed);
        let duration = video_frames.last().map(|f| f.pts_100ns + f.duration_100ns).unwrap_or(0)
            - video_frames[0].pts_100ns;
        let seg = CompletedSegment {
            path: output_path.to_string(),
            index: clip_idx,
            start_pts: video_frames[0].pts_100ns as f64 / 10_000_000.0,
            end_pts: video_frames.last().map(|f| (f.pts_100ns + f.duration_100ns) as f64 / 10_000_000.0).unwrap_or(0.0),
            duration: duration as f64 / 10_000_000.0,
        };
        self.saved_clips.lock().push(seg);

        Ok(output_path.to_string())
    }
}


/// Standalone save_clip implementation callable from any thread.
/// Decoupled from CaptureSession's &self to allow running on a background thread
/// without holding a borrow on Tauri State (which would block the async runtime).
pub(crate) fn save_clip_standalone(
    ring: &Arc<Mutex<EncodedMediaRing>>,
    audio_buffer: &Arc<Mutex<AudioRingBuffer>>,
    is_saving: &Arc<AtomicBool>,
    is_recording: &Arc<AtomicBool>,
    clip_counter: &Arc<AtomicU32>,
    saved_clips: &Arc<Mutex<Vec<CompletedSegment>>>,
    _multi_track_audio: &Arc<AtomicBool>,
    fps: u32,
    width: u32,
    height: u32,
    seconds: u32,
    output_path: &str,
) -> Result<String> {
    if !is_recording.load(Ordering::Relaxed) {
        eprintln!("[gpu_capture] save_clip_standalone: REJECTED — is_recording=false (capture may have died)");
        anyhow::bail!("Not recording — cannot save clip");
    }
    if is_saving.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
        anyhow::bail!("Save already in progress");
    }
    struct SavingGuard<'a>(&'a AtomicBool);
    impl<'a> Drop for SavingGuard<'a> {
        fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); }
    }
    let _guard = SavingGuard(is_saving);

    let multi_track = false; // Multi-track disabled
    let (video_frames, audio_chunks, mic_audio_chunks) = {
        let (video, start_pts, end_pts) = {
            let ring = ring.lock();
            let start_idx = ring
                .find_slice_start(seconds)
                .ok_or_else(|| anyhow::anyhow!("No keyframe found in ring buffer"))?;
            let video = ring.slice_video(start_idx);
            if video.is_empty() {
                anyhow::bail!("No video frames available for clip");
            }
            let s_pts = video[0].pts_100ns;
            let e_pts = video.last().map(|f| f.pts_100ns + f.duration_100ns).unwrap_or(s_pts);
            (video, s_pts, e_pts)
        };
        // Grab audio from separate audio buffer (zero contention with encoder)
        let audio = {
            let abuf = audio_buffer.lock();
            abuf.slice(start_pts, end_pts)
        };
        let mic_audio: Vec<AudioChunk> = Vec::new();

        (video, audio, mic_audio)
    };

    eprintln!("[gpu_capture] save_clip_standalone: {} video frames, {} audio chunks, {}x{} @ {}fps → {}",
        video_frames.len(), audio_chunks.len(), width, height, fps, output_path);

    let mic_for_mux: Option<&[AudioChunk]> = if multi_track && !mic_audio_chunks.is_empty() {
        Some(&mic_audio_chunks[..])
    } else {
        None
    };

    // catch_unwind to prevent MF crashes from killing the app
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { mux_to_mp4_ex(output_path, &video_frames, &audio_chunks, mic_for_mux, fps, width, height) }
    }));
    let result = match result {
        Ok(inner) => inner,
        Err(_) => Err(anyhow::anyhow!("Mux crashed — try again or switch to 720p")),
    };

    match &result {
        Ok(()) => eprintln!("[gpu_capture] save_clip_standalone: mux OK → {}", output_path),
        Err(e) => {
            eprintln!("[gpu_capture] save_clip_standalone: mux FAILED: {}", e);
            let _ = std::fs::remove_file(output_path);
        }
    }
    result?;

    let clip_idx = clip_counter.fetch_add(1, Ordering::Relaxed);
    let duration = video_frames.last().map(|f| f.pts_100ns + f.duration_100ns).unwrap_or(0)
        - video_frames[0].pts_100ns;
    let seg = CompletedSegment {
        path: output_path.to_string(),
        index: clip_idx,
        start_pts: video_frames[0].pts_100ns as f64 / 10_000_000.0,
        end_pts: video_frames.last().map(|f| (f.pts_100ns + f.duration_100ns) as f64 / 10_000_000.0).unwrap_or(0.0),
        duration: duration as f64 / 10_000_000.0,
    };
    saved_clips.lock().push(seg);
    Ok(output_path.to_string())
}

// ── WGC Capture Item Helpers ──────────────────────────────────────────────────

unsafe fn capture_item_from_monitor(hmon: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = interop.CreateForMonitor(hmon)?;
    Ok(item)
}

unsafe fn capture_item_from_window(hwnd: HWND) -> Result<GraphicsCaptureItem> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd)?;
    Ok(item)
}

// ── GPU Capture Loop with Dedicated Encoder Thread ────────────────────────────

/// Check if NVIDIA overlay/ShadowPlay processes are running that could conflict with capture.
/// Returns a warning message if conflicts are detected, None otherwise.
fn detect_nvidia_overlay_conflict() -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    // Check for known NVIDIA processes that hold DXGI capture sessions:
    // - "NVIDIA Share.exe" (ShadowPlay/Instant Replay)
    // - "nvcontainer.exe" with overlay modules
    // - "NvOAWrapperCache.exe" (overlay helper)
    let output = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq NVIDIA Share.exe", "/NH"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut conflicts = Vec::new();
    if stdout.contains("NVIDIA Share.exe") {
        conflicts.push("NVIDIA ShadowPlay/Share is running (Instant Replay may be active)");
    }

    // Also check for the newer NVIDIA App overlay
    let output2 = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq NvOAWrapperCache.exe", "/NH"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    if stdout2.contains("NvOAWrapperCache.exe") {
        conflicts.push("NVIDIA App overlay helper is running");
    }

    if conflicts.is_empty() {
        None
    } else {
        Some(format!(
            "Potential capture conflict detected:\n• {}\n\n\
            These processes may hold exclusive DXGI capture sessions. \
            If capture fails, disable Instant Replay in NVIDIA GeForce Experience/NVIDIA App.",
            conflicts.join("\n• ")
        ))
    }
}

fn run_gpu_capture(
    opts: CaptureOptions,
    stop: Arc<AtomicBool>,
    ring: Arc<Mutex<EncodedMediaRing>>,
    audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    mic_audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    ready_tx: std::sync::mpsc::Sender<Result<CaptureReadyInfo>>,
    frame_drops: Arc<AtomicU32>,
) -> Result<()> {
    let log = |_msg: &str| {};  // Disabled for production
    log("run_gpu_capture starting (dedicated encoder thread architecture)");

    // Resolve output dimensions from CaptureOptions (user's resolution setting)
    // Falls back to the OUTPUT_WIDTH/OUTPUT_HEIGHT constants if not specified.
    // When None (native resolution), we defer to the captured source dimensions (cap_w/cap_h).
    let requested_out_w = opts.target_width;
    let requested_out_h = opts.target_height;

    // Non-blocking check: warn about NVIDIA overlay conflicts (does NOT prevent capture)
    if let Some(warning) = detect_nvidia_overlay_conflict() {
        eprintln!("[gpu_capture] WARNING: {}", warning);
    }

    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    log("MFStartup OK");

    // Set system timer resolution to 1ms (same as games do).
    // Without this, Windows uses 15.6ms default which causes USB audio scheduling jitter
    // and mic distortion when no game is running (games set this themselves).
    unsafe {
        extern "system" { fn timeBeginPeriod(uPeriod: u32) -> u32; }
        timeBeginPeriod(1);
    }
    eprintln!("[gpu_capture] timeBeginPeriod(1) — 1ms timer resolution for USB audio");

    // Resolve target window/monitor
    let target_hwnd: Option<HWND> = match opts.source_id.as_deref() {
        Some(id) if id.starts_with("hwnd:") => {
            let v: usize = id[5..].parse().map_err(|e| anyhow::anyhow!("bad hwnd: {}", e))?;
            Some(HWND(v as *mut _))
        }
        _ => None,
    };

    let target_hmon: HMONITOR = unsafe {
        match target_hwnd {
            Some(hwnd) => {
                use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST;
                windows::Win32::Graphics::Gdi::MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
            }
            None => match opts.source_id.as_deref() {
                Some(id) if id.starts_with("monitor:") => {
                    let v: usize = id[8..].parse().map_err(|e| anyhow::anyhow!("bad monitor: {}", e))?;
                    HMONITOR(v as *mut _)
                }
                _ => MonitorFromPoint(
                    windows::Win32::Foundation::POINT { x: 0, y: 0 },
                    MONITOR_DEFAULTTOPRIMARY,
                ),
            },
        }
    };

    // Create D3D11 device on correct adapter (or use warm cache from app launch)
    let (device, context, winrt_device, gpu_vendor_id) = {
        // Try to use the warm cache (pre-created at app launch for fast start)
        // TODO: warm cache uses primary monitor adapter — if target is on a different adapter,
        // we need to create a fresh device. For now, always use the warm cache since most
        // gaming setups use a single GPU.
        let warm = opts.warm_cache.as_ref().and_then(|c| c.lock().take());
        if let Some(cache) = warm {
            eprintln!("[gpu_capture] Using warm-cached D3D11 device (saved ~100ms)");
            (cache.device, cache.context, cache.winrt_device, cache.gpu_vendor_id)
        } else {
            let matched_adapter = unsafe { find_adapter_for_monitor(target_hmon) };
            let (dev, ctx, wrt) = unsafe { create_d3d11_device(matched_adapter.as_ref())? };
            let vendor_id: u32 = unsafe {
                matched_adapter.as_ref()
                    .and_then(|a| a.GetDesc1().ok())
                    .map(|desc| desc.VendorId)
                    .unwrap_or(0)
            };
            (dev, ctx, wrt, vendor_id)
        }
    };
    log("D3D11 device ready");

    // GPU vendor for encoder tuning (already detected above)
    let is_amd = gpu_vendor_id == 0x1002;
    let _is_nvidia = gpu_vendor_id == 0x10DE;
    log(&format!("GPU vendor: 0x{:04X} (AMD={}, NVIDIA={})", gpu_vendor_id, is_amd, _is_nvidia));

    // Vendor-aware bitrate: AMD VCN produces more artifacts than NVENC at the same bitrate,
    // so we compensate with 50% more bits. This matches Radeon ReLive's "High" preset.
    let bitrate_kbps = if is_amd {
        // AMD: 50% higher bitrate to compensate for VCN's lower compression efficiency
        (opts.bitrate_kbps as f32 * 1.5) as u32
    } else {
        // NVIDIA: base bitrate is already tuned with headroom above ShadowPlay
        opts.bitrate_kbps
    };

    // Request capture access for protected content (CoD/RICOCHET anti-cheat).
    {
        use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
        let _ = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
            .and_then(|op| op.SetCompleted(None));
        let _ = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Programmatic)
            .and_then(|op| op.SetCompleted(None));
    }

    // Create capture item
    let item = unsafe {
        match target_hwnd {
            Some(hwnd) => capture_item_from_window(hwnd)?,
            None => capture_item_from_monitor(target_hmon)?,
        }
    };

    let size = item.Size()?;
    let cap_w = size.Width as u32;
    let cap_h = size.Height as u32;
    let fps = opts.fps;
    log(&format!("Capture item: {}x{} @ {}fps", cap_w, cap_h, fps));

    // Resolve final output dimensions: use requested or fall back to native capture size.
    // Ensure 16-pixel alignment (Clipsta Lite guardrail #2) — prevents AMD green rows.
    let out_w = (requested_out_w.unwrap_or(cap_w) + 15) & !15; // Round up to 16
    let out_h = (requested_out_h.unwrap_or(cap_h) + 15) & !15; // Round up to 16
    eprintln!("[gpu_capture] Output dimensions: {}x{} @ {}fps", out_w, out_h, fps);

    // Create Video Processor (BGRA→NV12 + scaling)
    let vp_state = unsafe {
        VideoProcessorState::new(&device, cap_w, cap_h, out_w, out_h, fps)?
    };
    let vp_state = Arc::new(parking_lot::RwLock::new(vp_state));
    log("VideoProcessor created");

    // Create NV12 pool pre-filled with legal black (AMD green fix)
    let nv12_pool = unsafe {
        create_nv12_pool(&device, &context, out_w, out_h, NV12_POOL_SIZE)?
    };
    log("NV12 pool created");

    // Initialize the persistent hardware H.264 encoder with fallback chain:
    // 1. Try optimal settings (High profile, L4.2, CBR, low latency)
    // 2. If that fails, try relaxed settings (Baseline profile, L4.0, VBR, no low-latency)
    // 3. If both fail, report the specific HRESULT with actionable guidance
    let (transform, event_gen) = {
        // Attempt 1: Optimal settings (High profile, adaptive level, CBR, low latency)
        match unsafe { init_hardware_encoder(&device, out_w, out_h, fps, bitrate_kbps) } {
            Ok(result) => {
                log("Hardware encoder initialized (optimal settings)");
                result
            }
            Err(e1) => {
                log(&format!("Encoder init attempt 1 (optimal) failed: {}", e1));
                // Attempt 2: Relaxed settings — Baseline profile, VBR, no low-latency
                match unsafe { init_hardware_encoder_relaxed(&device, out_w, out_h, fps, bitrate_kbps) } {
                    Ok(result) => {
                        log("Hardware encoder initialized (relaxed/fallback settings)");
                        eprintln!("[gpu_capture] WARNING: Using fallback encoder settings (Baseline profile). \
                            Optimal settings failed: {}. Update your GPU driver for best results.", e1);
                        result
                    }
                    Err(e2) => {
                        // Attempt 3: Bare minimum — no profile, no level, let driver decide everything
                        match unsafe { init_hardware_encoder_bare(&device, out_w, out_h, fps, bitrate_kbps) } {
                            Ok(result) => {
                                eprintln!("[gpu_capture] WARNING: Using bare-minimum encoder (no profile/level). \
                                    Attempt 1: {}. Attempt 2: {}.", e1, e2);
                                result
                            }
                            Err(e3) => {
                                let msg = format!(
                                    "Hardware H.264 encoder unavailable.\n\
                                    Attempt 1 (High profile): {}\n\
                                    Attempt 2 (Baseline fallback): {}\n\
                                    Attempt 3 (bare minimum): {}\n\n\
                                    Possible fixes:\n\
                                    • Update your GPU driver to the latest version\n\
                                    • Close NVIDIA ShadowPlay/Instant Replay if running\n\
                                    • Close any other screen recording software\n\
                                    • Restart your PC to release encoder sessions",
                                    e1, e2, e3
                                );
                                log(&format!("All encoder attempts failed"));
                                let _ = ready_tx.send(Err(anyhow::anyhow!("{}", msg)));
                                return Err(anyhow::anyhow!("{}", msg));
                            }
                        }
                    }
                }
            }
        }
    };

    // Create channel: WGC callback → encoder thread
    // SyncSender with bound=12 provides backpressure while allowing burst tolerance.
    // NV12 pool has 16 textures, so 12 in-flight is safe (4 free for WGC writes).
    // Provides ~200ms of burst tolerance when encoder stalls on keyframes.
    let (frame_tx, frame_rx): (SyncSender<FrameMsg>, Receiver<FrameMsg>) = mpsc::sync_channel(12);

    // Clone NV12 pool for encoder thread (Arc-wrapped for shared access)
    let nv12_pool_arc = Arc::new(nv12_pool);
    let nv12_pool_for_encoder = nv12_pool_arc.clone();

    // NV12 texture free-list: prevents race where VP overwrites a texture the encoder
    // hasn't finished consuming. Pre-filled with all indices; WGC callback takes one,
    // encoder thread returns it after the MFT signals it needs new input (confirming
    // the previous texture is no longer being read by the hardware encoder).
    let (nv12_free_tx, nv12_free_rx) = {
        let (tx, rx) = mpsc::sync_channel::<usize>(NV12_POOL_SIZE);
        for i in 0..NV12_POOL_SIZE {
            let _ = tx.send(i);
        }
        (tx, rx)
    };
    let nv12_free_rx = nv12_free_rx; // Owned directly by frame callback (single consumer)
    let nv12_free_tx_cb = nv12_free_tx.clone(); // Clone for callback to return unused textures

    // Spawn DEDICATED ENCODER THREAD
    let ring_for_encoder = ring.clone();
    let stop_for_encoder = stop.clone();
    let send_transform = SendTransform(transform);
    let send_event_gen = SendEventGen(event_gen);
    let send_textures = SendTextures((*nv12_pool_for_encoder).clone());
    let encoder_handle = thread::Builder::new()
        .name("clipsta-encoder".into())
        .spawn(move || {
            encoder_thread_fn(
                send_transform,
                send_event_gen,
                send_textures,
                frame_rx,
                ring_for_encoder,
                stop_for_encoder,
                fps,
                nv12_free_tx,
            );
        })?;
    log("Dedicated encoder thread spawned");

    // Create frame pool for WGC
    // Try BGRA8 first (standard SDR). If the system has HDR active and this fails,
    // fall back to R16G16B16A16Float which is the HDR desktop format.
    // The video processor will handle the color space conversion to NV12 BT.709.
    // Frame pool buffer count: 3 gives WGC a spare buffer while VP is processing,
    // preventing DWM back-pressure stalls that cause game hitches. (Was 2, which
    // caused DWM to block when VP took >16ms under GPU load.)
    let (frame_pool, capture_pixel_format) = {
        match Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            3,
            size,
        ) {
            Ok(pool) => (pool, DirectXPixelFormat::B8G8R8A8UIntNormalized),
            Err(_) => {
                // HDR desktop: try 16-bit float format
                let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                    &winrt_device,
                    DirectXPixelFormat::R16G16B16A16Float,
                    3,
                    size,
                )?;
                (pool, DirectXPixelFormat::R16G16B16A16Float)
            }
        }
    };

    let session = frame_pool.CreateCaptureSession(&item)?;
    session.SetIsCursorCaptureEnabled(true)?;
    let _ = session.SetIsBorderRequired(false);

    // Cap WGC frame delivery at target fps. Without this, WGC delivers at the
    // display's refresh rate (e.g., 144Hz), wasting GPU time on frames we don't need.
    // On 60Hz monitors this is a no-op, but it prevents overhead on high-refresh displays
    // and slightly reduces GPU contention that causes encoder stalls.
    let frame_interval_100ns = 10_000_000i64 / fps as i64; // 166666 for 60fps
    let _ = session.SetMinUpdateInterval(TimeSpan { Duration: frame_interval_100ns });

    // Handle capture target closing
    {
        let stop_on_close = stop.clone();
        item.Closed(&TypedEventHandler::new(move |_, _| {
            stop_on_close.store(true, Ordering::SeqCst);
            Ok(())
        }))?;
    }

    // Send ready info
    eprintln!("[gpu_capture] Pipeline ready: {}x{} @ {}fps, encoder initialized successfully", out_w, out_h, fps);
    let ready_info = CaptureReadyInfo {
        width: out_w,
        height: out_h,
        fps,
        segment_dir: opts.segment_dir.to_string_lossy().to_string(),
    };
    let _ = ready_tx.send(Ok(ready_info));

    // Audio thread — uses a shared wall-clock reference for A/V sync.
    // OnceLock<Instant> is lock-free after the first frame sets it.
    // Both video PTS and audio use (now - session_start) ensuring perfect sync.
    let session_start: Arc<std::sync::OnceLock<std::time::Instant>> = Arc::new(std::sync::OnceLock::new());
    let audio_thread = if !opts.no_audio {
        let audio_buf = audio_buffer.clone();
        let mic_buf = mic_audio_buffer.clone();
        let s = stop.clone();
        let ss = session_start.clone();
        let mic = opts.mic_device.clone();
        let lb = opts.loopback_device.clone();
        // Multi-track only when the user opted in AND a mic is configured.
        // Default (false) keeps the proven single mixed track (mic mixed into desktop).
        let multi_track = opts.multi_track_audio && opts.mic_device.is_some();
        Some(thread::spawn(move || {
            gpu_audio_loop(s, mic, lb, audio_buf, mic_buf, ss, multi_track);
        }))
    } else {
        None
    };

    // Track capture size for resize detection
    let cap_size = Arc::new((AtomicU32::new(cap_w), AtomicU32::new(cap_h)));

    // Frame counter for debug logging
    let frame_counter = Arc::new(AtomicUsize::new(0));

    // NV12 texture free-list: prevents race where VP overwrites a texture the encoder
    // hasn't finished consuming. Pre-filled with all indices; WGC callback takes one,
    // encoder thread returns it after the MFT signals it needs new input (confirming
    // Track last successfully sent NV12 pool index for frame-repeat-on-drop
    let last_sent_idx = Arc::new(AtomicUsize::new(usize::MAX));

    // Frame pacing: enforce target fps even when WGC delivers faster.
    // AtomicI64 avoids mutex overhead on the hot path (60+ times/sec).
    // When game runs at >60fps, WGC may still deliver excess frames despite
    // SetMinUpdateInterval (which is advisory). This enforces the cap.
    let last_accepted_ns = Arc::new(AtomicI64::new(0));

    // Adaptive VP skip: when VideoProcessorBlt takes longer than the frame interval,
    // the GPU is under pressure from the game. Skip the next VP call to give the GPU
    // breathing room. This gracefully degrades recording to momentary 30fps rather
    // than causing game FPS drops. Resets automatically once GPU pressure eases.
    let vp_skip_next = Arc::new(AtomicBool::new(false));

    // Frame arrived callback — MUST NOT BLOCK
    let stop_cb = stop.clone();
    let device_cb = device.clone();
    let vp_state_cb = vp_state.clone();
    let cap_size_cb = cap_size.clone();
    let frame_counter_cb = frame_counter.clone();
    let nv12_free_rx_cb = nv12_free_rx; // Moved into closure (single consumer, no Arc needed)
    let nv12_free_tx_cb = nv12_free_tx_cb; // Moved into closure for returning unused textures
    let session_start_cb = session_start.clone();
    let nv12_pool_cb = nv12_pool_arc.clone();
    let last_sent_idx_cb = last_sent_idx.clone();
    let frame_drops_cb = frame_drops.clone();
    let last_accepted_ns_cb = last_accepted_ns.clone();
    let vp_skip_next_cb = vp_skip_next.clone();

    struct SendDevice(IDirect3DDevice);
    unsafe impl Send for SendDevice {}
    unsafe impl Sync for SendDevice {}
    let winrt_device_cb = Arc::new(SendDevice(winrt_device.clone()));

    frame_pool.FrameArrived(&TypedEventHandler::new({
        move |pool: windows_core::Ref<Direct3D11CaptureFramePool>, _| {
            if stop_cb.load(Ordering::Relaxed) {
                return Ok(());
            }

            let pool_ref = pool.ok()?;
            let frame = match pool_ref.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => return Ok(()),
            };

            // Set session start time on first frame (shared with audio for A/V sync).
            // OnceLock: lock-free after first call — no mutex overhead on subsequent frames.
            let _ = session_start_cb.get_or_init(|| std::time::Instant::now());

            // Get D3D11 texture from WGC frame
            let surface = frame.Surface()?;
            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
            let frame_texture: ID3D11Texture2D = unsafe { access.GetInterface()? };

            // Detect resize
            if let Ok(content_size) = frame.ContentSize() {
                let (new_w, new_h) = (content_size.Width as u32, content_size.Height as u32);
                let (old_w, old_h) = (
                    cap_size_cb.0.load(Ordering::Relaxed),
                    cap_size_cb.1.load(Ordering::Relaxed),
                );
                if (new_w != old_w && new_w > 0) || (new_h != old_h && new_h > 0) {
                    match pool_ref.Recreate(
                        &winrt_device_cb.0,
                        capture_pixel_format,
                        3,
                        content_size,
                    ) {
                        Ok(()) => {
                            cap_size_cb.0.store(new_w, Ordering::Relaxed);
                            cap_size_cb.1.store(new_h, Ordering::Relaxed);
                            let mut vp = vp_state_cb.write();
                            let _ = unsafe { vp.update_source_size(&device_cb, new_w, new_h, out_w, out_h, fps) };
                        }
                        Err(e) => eprintln!("[gpu_capture] Recreate failed: {e}"),
                    }
                }
            }

            // Calculate PTS — wall-clock elapsed (Clipsta Lite approach).
            // Both video and audio use session_start.elapsed() ensuring perfect A/V sync.
            let duration_100ns = 10_000_000i64 / fps as i64;
            let frame_num = frame_counter_cb.fetch_add(1, Ordering::Relaxed) as i64;
            let pts_100ns = match session_start_cb.get() {
                Some(start) => start.elapsed().as_nanos() as i64 / 100,
                None => frame_num * duration_100ns, // fallback before first frame
            };

            // Frame pacing: only reject true duplicate frames (< 2ms apart).
            // Since PTS is wall-clock based, we don't need grid-based pacing.
            // WGC sometimes delivers the same frame twice in quick succession.
            let pacing_now = match session_start_cb.get() {
                Some(start) => start.elapsed().as_nanos() as i64 / 100,
                None => 0,
            };
            let min_interval = 20_000i64; // 2ms — only reject true duplicates
            {
                let last = last_accepted_ns_cb.load(Ordering::Relaxed);
                if pacing_now - last < min_interval && last != 0 {
                    // Undo frame counter since we're rejecting this frame
                    frame_counter_cb.fetch_sub(1, Ordering::Relaxed);
                    return Ok(());
                }
                last_accepted_ns_cb.store(pacing_now, Ordering::Relaxed);
            }

            // Acquire a free NV12 texture from the pool.
            // If none available, encoder hasn't released any — skip this frame
            // (game performance takes priority over recording completeness).
            let pool_idx = match nv12_free_rx_cb.try_recv() {
                Ok(idx) => idx,
                Err(_) => {
                    // All textures in use by encoder — skip frame, send repeat
                    let repeat_idx = last_sent_idx_cb.load(Ordering::Relaxed);
                    if repeat_idx != usize::MAX {
                        let msg = FrameMsg {
                            texture_index: repeat_idx,
                            pts_100ns,
                            duration_100ns,
                        };
                        let _ = frame_tx.try_send(msg);
                    }
                    frame_drops_cb.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
            };
            let nv12_tex = &nv12_pool_cb[pool_idx];

            // Adaptive VP skip: if the previous VP call was slow (GPU under pressure),
            // skip this frame to give the game's rendering pipeline breathing room.
            // The encoder thread will duplicate the previous frame via its timeout logic,
            // so the output stays at 60fps with a repeated frame — no gap.
            if vp_skip_next_cb.swap(false, Ordering::Relaxed) {
                // Return the unused pool texture — we're skipping VP this frame
                let _ = nv12_free_tx_cb.try_send(pool_idx);
                // Send a repeat of the last good texture instead
                let repeat_idx = last_sent_idx_cb.load(Ordering::Relaxed);
                if repeat_idx != usize::MAX {
                    let msg = FrameMsg {
                        texture_index: repeat_idx,
                        pts_100ns,
                        duration_100ns,
                    };
                    let _ = frame_tx.try_send(msg);
                }
                return Ok(());
            }

            // VideoProcessor: BGRA→NV12 + scale to 1280x720 (GPU, fast)
            // Time the VP call to detect GPU pressure.
            let vp_start = std::time::Instant::now();
            {
                let vp = vp_state_cb.read();
                if let Err(e) = unsafe { vp.process(&frame_texture, nv12_tex) } {
                    eprintln!("[gpu_capture] VP process failed: {e}");
                    return Ok(());
                }
            }
            let vp_elapsed_us = vp_start.elapsed().as_micros() as i64;

            // If VP took longer than 2 frame intervals, GPU is truly saturated — skip next frame.
            // This prevents Clipsta from competing with the game for GPU time.
            // Previous threshold (1x = 16.6ms) was too aggressive at 1080p where VP
            // routinely takes 17-20ms under moderate GPU load (BF6, etc.), causing
            // steady-state drops to ~54fps. 2x threshold (33ms) only triggers when
            // the GPU is genuinely overloaded, preserving 60fps in normal gameplay.
            let frame_interval_us = 1_000_000i64 / fps as i64;
            if vp_elapsed_us > frame_interval_us * 2 {
                vp_skip_next_cb.store(true, Ordering::Relaxed);
            }

            // Send frame message to encoder thread — DOES NOT BLOCK.
            // If the channel is full (encoder behind), repeat the last successfully
            // sent frame's texture at the current PTS. This ensures the encoder always
            // receives 60fps worth of frames — dropped captures become duplicates
            // rather than gaps, so the output MP4 is always 60fps.
            let msg = FrameMsg {
                texture_index: pool_idx,
                pts_100ns,
                duration_100ns,
            };
            match frame_tx.try_send(msg) {
                Ok(()) => {
                    // Success — remember this pool index for potential repeat
                    last_sent_idx_cb.store(pool_idx, Ordering::Relaxed);
                }
                Err(mpsc::TrySendError::Full(dropped_msg)) => {
                    // Channel full: encoder is behind. Return the unused texture to
                    // the free pool (prevents pool exhaustion over time), then send a
                    // repeat of the last successfully sent texture instead.
                    let _ = nv12_free_tx_cb.try_send(pool_idx);
                    frame_drops_cb.fetch_add(1, Ordering::Relaxed);
                    let repeat_idx = last_sent_idx_cb.load(Ordering::Relaxed);
                    if repeat_idx != usize::MAX {
                        let repeat_msg = FrameMsg {
                            texture_index: repeat_idx,
                            pts_100ns: dropped_msg.pts_100ns,
                            duration_100ns: dropped_msg.duration_100ns,
                        };
                        // Best-effort repeat — if still full, accept the drop
                        let _ = frame_tx.try_send(repeat_msg);
                    }
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    // Encoder thread exited — stop will be set soon
                }
            }

            Ok(())
        }
    }))?;

    // Start capture
    session.StartCapture()?;
    log("StartCapture() called - frames should begin arriving");

    // Wait for stop signal
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(50));
    }

    // Stop capture
    session.Close()?;
    frame_pool.Close()?;

    // Wait for encoder thread to finish (it will drain on stop)
    let _ = encoder_handle.join();
    log("Encoder thread joined");

    // Wait for audio thread
    if let Some(t) = audio_thread {
        let _ = t.join();
    }

    // Restore default timer resolution
    unsafe {
        extern "system" { fn timeEndPeriod(uPeriod: u32) -> u32; }
        timeEndPeriod(1);
    }

    unsafe {
        MFShutdown()?;
    }
    log("run_gpu_capture finished");
    Ok(())
}


// ── Audio Capture Loop ────────────────────────────────────────────────────────

/// Initialize a Media Foundation AAC encoder transform for real-time encoding.
/// Returns the IMFTransform on success, or None if initialization fails
/// (in which case we fall back to PCM-only mode — AAC encoded at save time).
unsafe fn init_aac_encoder_mft() -> Option<IMFTransform> {
    // Find AAC encoder MFT
    let category = MFT_CATEGORY_AUDIO_ENCODER;
    let mut count = 0u32;
    let mut activates: *mut Option<IMFActivate> = ptr::null_mut();

    let out_type: IMFMediaType = MFCreateMediaType().ok()?;
    out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
    out_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ok()?;

    MFTEnumEx(
        category,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        None,
        Some(&MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Audio,
            guidSubtype: MFAudioFormat_AAC,
        }),
        &mut activates,
        &mut count,
    ).ok()?;

    if count == 0 || activates.is_null() {
        return None;
    }

    let activate_slice = std::slice::from_raw_parts(activates, count as usize);
    let transform: IMFTransform = activate_slice[0].as_ref()?.ActivateObject().ok()?;

    // Release all IMFActivate COM objects before freeing the array.
    {
        let activates_owned = std::slice::from_raw_parts_mut(activates, count as usize);
        for slot in activates_owned.iter_mut() {
            let _ = slot.take(); // Drop calls Release()
        }
    }
    CoTaskMemFree(Some(activates as *const _));

    // Set input type: PCM f32 → 16-bit PCM (MF AAC encoder expects 16-bit)
    let in_type: IMFMediaType = MFCreateMediaType().ok()?;
    in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
    in_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ok()?;
    in_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ok()?;
    in_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
    in_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
    in_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGN).ok()?;
    in_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_SAMPLE_RATE * AUDIO_BLOCK_ALIGN).ok()?;
    transform.SetInputType(0, &in_type, 0).ok()?;

    // Set output type: AAC
    let out_type2: IMFMediaType = MFCreateMediaType().ok()?;
    out_type2.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
    out_type2.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ok()?;
    out_type2.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE).ok()?;
    out_type2.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS).ok()?;
    out_type2.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ok()?;
    out_type2.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000).ok()?; // 192 kbps
    out_type2.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1).ok()?;
    let _ = out_type2.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29);
    transform.SetOutputType(0, &out_type2, 0).ok()?;

    // Notify the encoder to start processing
    transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).ok()?;
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ok()?;
    transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ok()?;

    Some(transform)
}

/// Feed PCM samples (as i16) to the AAC encoder and collect any output.
/// Returns encoded AAC chunks ready to push to the ring.
unsafe fn feed_aac_encoder(
    transform: &IMFTransform,
    pcm_i16: &[i16],
    pts_100ns: i64,
    duration_100ns: i64,
) -> Vec<EncodedAudioChunk> {
    let mut output_chunks = Vec::new();

    // Create input sample
    let byte_len = (pcm_i16.len() * 2) as u32;
    let Ok(buf) = MFCreateMemoryBuffer(byte_len) else { return output_chunks; };
    let mut p: *mut u8 = ptr::null_mut();
    if buf.Lock(&mut p, None, None).is_err() { return output_chunks; }
    ptr::copy_nonoverlapping(pcm_i16.as_ptr() as *const u8, p, byte_len as usize);
    let _ = buf.Unlock();
    let _ = buf.SetCurrentLength(byte_len);

    let Ok(sample) = MFCreateSample() else { return output_chunks; };
    let _ = sample.AddBuffer(&buf);
    let _ = sample.SetSampleTime(pts_100ns);
    let _ = sample.SetSampleDuration(duration_100ns);

    // Feed input to the encoder
    let _ = transform.ProcessInput(0, &sample, 0);

    // Drain any available output
    loop {
        let output_info = match transform.GetOutputStreamInfo(0) {
            Ok(info) => info,
            Err(_) => break,
        };

        let out_buf_size = output_info.cbSize.max(8192);
        let Ok(out_buf) = MFCreateMemoryBuffer(out_buf_size) else { break; };
        let Ok(out_sample) = MFCreateSample() else { break; };
        let _ = out_sample.AddBuffer(&out_buf);

        let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(Some(out_sample.clone())),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        };
        let mut status = 0u32;

        let hr = transform.ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &mut status);
        if hr.is_err() {
            break; // No more output available (MF_E_TRANSFORM_NEED_MORE_INPUT)
        }

        // Extract the encoded data
        if let Ok(out_buf2) = out_sample.ConvertToContiguousBuffer() {
            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut cur_len = 0u32;
            if out_buf2.Lock(&mut data_ptr, None, Some(&mut cur_len)).is_ok() && cur_len > 0 {
                let encoded_data = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
                let _ = out_buf2.Unlock();

                let out_pts = out_sample.GetSampleTime().unwrap_or(pts_100ns);
                let out_dur = out_sample.GetSampleDuration().unwrap_or(duration_100ns);

                output_chunks.push(EncodedAudioChunk {
                    data: Arc::new(encoded_data),
                    pts_100ns: out_pts,
                    duration_100ns: out_dur,
                });
            } else {
                let _ = out_buf2.Unlock();
            }
        }
    }

    output_chunks
}

/// Audio capture loop: captures 48kHz stereo PCM and pushes to the ring buffer.
///
/// Single-track mode (default): desktop + mic are mixed into one track (`audio_buffer`).
/// Multi-track mode: desktop-only goes to `audio_buffer`, mic-only goes to
/// `mic_audio_buffer`, and the mux writes them as two separate MP4 audio tracks.
fn gpu_audio_loop(
    stop: Arc<AtomicBool>,
    mic_device: Option<String>,
    loopback: Option<String>,
    audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    mic_audio_buffer: Arc<Mutex<AudioRingBuffer>>,
    session_start: Arc<std::sync::OnceLock<std::time::Instant>>,
    multi_track_audio: bool,
) {
    unsafe {
        use windows::Win32::System::Threading::*;
        // Set thread to TIME_CRITICAL — highest non-realtime priority.
        // This ensures the audio thread gets scheduled even when WebView2's
        // background threads are consuming CPU.
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    loop {
        if stop.load(Ordering::Relaxed) { return; }
        if session_start.get().is_some() { break; }
        thread::sleep(std::time::Duration::from_millis(1));
    }
    eprintln!("[gpu_audio] Lock-free audio (multi_track={})", multi_track_audio);

    // Channel item: (samples, pts_100ns, duration_100ns, is_mic_track).
    // is_mic_track routes to the separate mic buffer in multi-track mode.
    let (tx, rx) = std::sync::mpsc::sync_channel::<(Vec<f32>, i64, i64, bool)>(400);

    let stop_clone = stop.clone();
    let session_start_clone = session_start.clone();

    // Spawn WASAPI capture on a dedicated thread with TIME_CRITICAL priority
    let capture_handle = thread::spawn(move || {
        unsafe {
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }
        let pts_now = {
            let ss = session_start_clone.clone();
            move || match ss.get() {
                Some(start) => start.elapsed().as_nanos() as i64 / 100,
                None => 0,
            }
        };

        if multi_track_audio {
            // Multi-track: desktop-only to the main track, mic-only to the mic track.
            let tx_main = tx.clone();
            let pts_main = pts_now.clone();
            let tx_mic = tx.clone();
            let pts_mic = pts_now.clone();
            let res = WasapiCapture::capture_to_callback_multi(
                stop_clone,
                mic_device,
                loopback,
                move |chunk: &[f32]| {
                    let n_frames = chunk.len() / AUDIO_CHANNELS as usize;
                    let duration_100ns = (n_frames as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;
                    let _ = tx_main.try_send((chunk.to_vec(), pts_main(), duration_100ns, false));
                },
                Some(move |chunk: &[f32]| {
                    let n_frames = chunk.len() / AUDIO_CHANNELS as usize;
                    let duration_100ns = (n_frames as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;
                    let _ = tx_mic.try_send((chunk.to_vec(), pts_mic(), duration_100ns, true));
                }),
            );
            if let Err(e) = res {
                eprintln!("[gpu_audio] WASAPI (multi-track) error: {e}");
            }
        } else {
            // Single-track: desktop + mic mixed into one stream (proven default).
            let res = WasapiCapture::capture_to_callback(stop_clone, mic_device, loopback, move |chunk: &[f32]| {
                let n_frames = chunk.len() / AUDIO_CHANNELS as usize;
                let duration_100ns = (n_frames as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;
                let _ = tx.try_send((chunk.to_vec(), pts_now(), duration_100ns, false));
            });
            if let Err(e) = res {
                eprintln!("[gpu_audio] WASAPI error: {e}");
            }
        }
    });

    // Consumer loop: pull audio from channel and push to the appropriate ring buffer.
    // This thread can safely lock the buffers because it's NOT the WASAPI callback thread.
    let push_chunk = |buf: &Arc<Mutex<AudioRingBuffer>>, samples: &[f32], pts: i64, dur: i64| {
        let mut abuf = buf.lock();
        let mut b = abuf.acquire_buffer(samples.len());
        b.extend_from_slice(samples);
        abuf.push(AudioChunk { data: Arc::new(b), pts_100ns: pts, duration_100ns: dur });
    };

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok((samples, pts, dur, is_mic)) => {
                if is_mic {
                    push_chunk(&mic_audio_buffer, &samples, pts, dur);
                } else {
                    push_chunk(&audio_buffer, &samples, pts, dur);
                }
                // Drain any additional pending chunks
                while let Ok((samples, pts, dur, is_mic)) = rx.try_recv() {
                    if is_mic {
                        push_chunk(&mic_audio_buffer, &samples, pts, dur);
                    } else {
                        push_chunk(&audio_buffer, &samples, pts, dur);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = capture_handle.join();
}

// ── Capture Diagnostics ───────────────────────────────────────────────────────

/// Diagnostics info for troubleshooting capture issues.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDiagnostics {
    pub gpu_adapter: String,
    pub driver_version: String,
    pub hdr_active: bool,
    pub encoder_available: bool,
    pub encoder_name: String,
    pub nvidia_overlay_running: bool,
    pub conflict_warning: Option<String>,
}

/// Run capture diagnostics: checks GPU adapter, driver, HDR state, encoder availability.
/// This is purely informational — does not modify any state or start capture.
pub fn capture_diagnostics() -> CaptureDiagnostics {
    let mut diag = CaptureDiagnostics {
        gpu_adapter: "Unknown".to_string(),
        driver_version: "Unknown".to_string(),
        hdr_active: false,
        encoder_available: false,
        encoder_name: "None".to_string(),
        nvidia_overlay_running: false,
        conflict_warning: None,
    };

    // GPU adapter info via DXGI
    unsafe {
        if let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() {
            if let Ok(adapter) = factory.EnumAdapters1(0) {
                if let Ok(desc) = adapter.GetDesc1() {
                    let name_len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
                    diag.gpu_adapter = String::from_utf16_lossy(&desc.Description[..name_len]);

                    // Driver version from adapter LUID — query via registry is more reliable
                    // but the DXGI dedicated video memory + vendor ID is useful context
                    let vendor = desc.VendorId;
                    let device = desc.DeviceId;
                    diag.driver_version = format!("VendorID=0x{:04X} DeviceID=0x{:04X}", vendor, device);
                }
            }
        }
    }

    // Try to get driver version from DXGIAdapter (version available via CheckInterfaceSupport on older APIs)
    // More reliable: use dxdiag-style registry query
    {
        use std::os::windows::process::CommandExt;
        if let Ok(output) = std::process::Command::new("wmic")
            .args(["path", "Win32_VideoController", "get", "DriverVersion", "/value"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(ver_line) = stdout.lines().find(|l| l.starts_with("DriverVersion=")) {
                diag.driver_version = ver_line.trim_start_matches("DriverVersion=").trim().to_string();
            }
        }
    }

    // HDR state: check if the primary monitor has AdvancedColorInfo active
    // Simplest check: try creating a frame pool with BGRA — if it fails, HDR might be forcing 16-bit
    // More accurate: check Windows display settings via registry
    {
        use std::os::windows::process::CommandExt;
        if let Ok(output) = std::process::Command::new("reg")
            .args(["query", r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\VideoSettings", "/v", "EnableHDRForDisplay"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            diag.hdr_active = stdout.contains("0x1");
        }
    }

    // Encoder availability: try MFTEnumEx without creating a session
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);

        let flags = MFT_ENUM_FLAG(
            MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
        );
        let in_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let out_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };

        let mut activates_ptr: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count: u32 = 0;
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&in_info),
            Some(&out_info),
            &mut activates_ptr,
            &mut count,
        ).is_ok() && count > 0 && !activates_ptr.is_null() {
            diag.encoder_available = true;
            diag.encoder_name = format!("H.264 Hardware Encoder ({} found)", count);
            CoTaskMemFree(Some(activates_ptr as *const _));
        }

        let _ = MFShutdown();
    }

    // NVIDIA overlay check
    diag.conflict_warning = detect_nvidia_overlay_conflict();
    diag.nvidia_overlay_running = diag.conflict_warning.is_some();

    diag
}

// ── Source Listing ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub source_type: String,
    pub width: i32,
    pub height: i32,
}

pub fn list_sources() -> Vec<SourceInfo> {
    let mut sources: Vec<SourceInfo> = Vec::new();
    unsafe {
        use windows::Win32::Foundation::{LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::*;
        use windows_core::BOOL;

        extern "system" fn mon_cb(
            hmon: HMONITOR,
            _: HDC,
            _: *mut RECT,
            lp: LPARAM,
        ) -> BOOL {
            let list = unsafe { &mut *(lp.0 as *mut Vec<SourceInfo>) };
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() } {
                let name = String::from_utf16_lossy(
                    &info.szDevice.iter().take_while(|&&c| c != 0).cloned().collect::<Vec<_>>(),
                );
                let r = &info.monitorInfo.rcMonitor;
                list.push(SourceInfo {
                    id: format!("monitor:{}", hmon.0 as usize),
                    name: format!("Display {}", name.trim()),
                    source_type: "monitor".into(),
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                });
            }
            BOOL(1)
        }

        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(mon_cb),
            LPARAM(&mut sources as *mut _ as isize),
        );

        use windows::Win32::UI::WindowsAndMessaging::*;

        extern "system" fn win_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
            let list = unsafe { &mut *(lp.0 as *mut Vec<SourceInfo>) };
            if !unsafe { IsWindowVisible(hwnd).as_bool() } {
                return BOOL(1);
            }
            let mut t = [0u16; 512];
            let len = unsafe { GetWindowTextW(hwnd, &mut t) };
            if len == 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&t[..len as usize]);
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = unsafe { GetWindowRect(hwnd, &mut r) };
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if w < 150 || h < 150 {
                return BOOL(1);
            }
            list.push(SourceInfo {
                id: format!("hwnd:{}", hwnd.0 as usize),
                name: title,
                source_type: "window".into(),
                width: w,
                height: h,
            });
            BOOL(1)
        }

        let _ = EnumWindows(Some(win_cb), LPARAM(&mut sources as *mut _ as isize));
    }
    sources
}
