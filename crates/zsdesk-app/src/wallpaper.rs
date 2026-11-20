//! Wallpaper layer: one picture decoded, blurred and dimmed for the current
//! window size, then drawn behind every other layer.
//!
//! The decode is expensive (disk read plus a full-frame blur), so the result is
//! kept until the path, the window size or one of the sliders changes. The
//! bitmap belongs to the render target generation, exactly like icons: a
//! recreated target drops it and the picture is uploaded again from the WIC
//! copy that is still in memory.

use std::path::PathBuf;

use windows::Win32::Graphics::{Direct2D::ID2D1Bitmap, Imaging::IWICBitmapSource};
use zsdesk_core::model::explorer::WallpaperPreferences;
use zsdesk_windows::gfx::{Canvas, Gfx, RectF};

/// Everything the decoded picture depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WallpaperKey {
    path: PathBuf,
    width: i32,
    height: i32,
    blur: u8,
    dim: u8,
}

pub(crate) struct WallpaperLayer {
    key: Option<WallpaperKey>,
    /// Source pixels kept so a render-target reset does not decode again.
    source: Option<IWICBitmapSource>,
    bitmap: Option<(u64, ID2D1Bitmap)>,
    /// Mean luminance of the decoded picture, 0..=1.
    mean: f32,
    /// A path that failed to decode is remembered so the attempt runs once.
    failed: Option<WallpaperKey>,
}

impl Default for WallpaperLayer {
    fn default() -> Self { Self { key: None, source: None, bitmap: None, mean: 0.0, failed: None } }
}

impl WallpaperLayer {
    /// Returns the picture for this frame, decoding it only when something that
    /// affects it changed.
    pub(crate) fn prepare(&mut self, gfx: &Gfx, width: i32, height: i32, prefs: &WallpaperPreferences) -> Option<(ID2D1Bitmap, f32)> {
        let opacity = prefs.opacity as f32 / 100.0;
        let path = prefs.path.as_ref()?;
        if opacity <= 0.0 || width <= 0 || height <= 0 { return None; }
        // The blur runs on the CPU, so the picture is decoded below the window
        // size and stretched when drawn: dragging a window edge would otherwise
        // start a full decode and blur for every pixel of the drag.
        let (width, height) = decode_size(width, height);
        let key = WallpaperKey { path: path.clone(), width, height, blur: prefs.blur.min(100), dim: prefs.dim.min(100) };
        if self.failed.as_ref() == Some(&key) { return None; }
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key.clone());
            self.bitmap = None;
            match gfx.wallpaper(path, width, height, key.blur, key.dim) {
                Some((source, mean)) => { self.source = Some(source); self.mean = mean; }
                None => {
                    self.source = None;
                    self.failed = Some(key);
                    return None;
                }
            }
        }
        let generation = gfx.generation();
        if self.bitmap.as_ref().is_none_or(|(cached, _)| *cached != generation) {
            self.bitmap = self.source.as_ref().and_then(|source| gfx.upload(source)).map(|bitmap| (generation, bitmap));
        }
        self.bitmap.as_ref().map(|(_, bitmap)| (bitmap.clone(), opacity))
    }

    /// Mean luminance of the picture currently on screen, 0..=1.
    pub(crate) fn mean(&self) -> f32 { self.mean }

    /// Forgets the decoded picture, e.g. after the user edited the preference.
    pub(crate) fn invalidate(&mut self) {
        self.key = None;
        self.source = None;
        self.bitmap = None;
        self.mean = 0.0;
        self.failed = None;
    }
}

/// Largest edge the picture is decoded at, and the step its size is rounded to.
const MAX_DECODE: i32 = 1600;
const DECODE_STEP: i32 = 16;

/// Size the picture is decoded at. Rounding keeps the cache key stable while a
/// window is dragged, and the cap bounds the CPU blur.
fn decode_size(width: i32, height: i32) -> (i32, i32) {
    let longest = width.max(height).max(1);
    let scale = (MAX_DECODE as f32 / longest as f32).min(1.0);
    let round = |value: i32| ((value as f32 * scale) as i32 / DECODE_STEP * DECODE_STEP).max(DECODE_STEP);
    (round(width), round(height))
}

/// Draws the layer over `rect`. Transparent pixels keep the DWM material.
pub(crate) fn paint(cv: &Canvas, rect: RectF, layer: Option<&(ID2D1Bitmap, f32)>) {
    if let Some((bitmap, opacity)) = layer {
        cv.image(bitmap, rect, *opacity);
    }
}

#[cfg(test)]
mod tests {
    use super::decode_size;

    #[test]
    fn decode_size_caps_large_windows_and_rounds_the_key() {
        assert_eq!(decode_size(1280, 800), (1280, 800));
        // 2560x1440 scales by 0.625 and lands on the 16 px grid.
        assert_eq!(decode_size(2560, 1440), (1600, 896));
        assert_eq!(decode_size(0, 0), (16, 16));
    }
}
