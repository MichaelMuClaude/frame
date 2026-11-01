//! HEIC/HEIF decoding on Windows through the Windows Imaging Component, the same codec the
//! Photos app uses. The codec comes from the "HEIF Image Extensions" and "HEVC Video Extensions"
//! in the Microsoft Store, so frame does not have to ship an image library of its own.

use std::path::Path;

use windows::Win32::Foundation::{GENERIC_READ, WINCODEC_ERR_COMPONENTNOTFOUND};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::HSTRING;

use crate::loader::HEIF_CODEC_MISSING;

pub fn decode(path: &Path) -> Result<image::RgbaImage, String> {
    // Each call balances its own COM setup, so the decoder threads need none.
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let result = unsafe { decode_rgba(path) }.map_err(|e| {
        if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND { HEIF_CODEC_MISSING.to_string() } else { e.to_string() }
    });
    if init.is_ok() {
        unsafe { CoUninitialize() };
    }
    result
}

unsafe fn decode_rgba(path: &Path) -> windows::core::Result<image::RgbaImage> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder =
            factory.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let rgba = factory.CreateFormatConverter()?;
        rgba.Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let (mut w, mut h) = (0, 0);
        rgba.GetSize(&mut w, &mut h)?;
        let mut buf = vec![0; w as usize * h as usize * 4];
        rgba.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
        Ok(image::RgbaImage::from_raw(w, h, buf).expect("buffer matches the size"))
    }
}
