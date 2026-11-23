//! Wallpaper layer: one picture decoded, blurred and dimmed for the current
//! window size, then drawn behind every other layer.
//!
//! The decode is expensive (disk read, JPEG decode, format conversion), so it
//! happens once per picture and lives in a base bitmap. Every window size is
//! then derived from that base, which is cheap enough to run while a window
//! edge is dragged: decoding per size costs tens of milliseconds and turns a
//! resize into a slideshow.
//!
//! Bitmaps belong to the render target generation, exactly like icons: a
//! recreated target drops them and the picture is uploaded again from the WIC
//! copies that are still in memory.

use std::path::PathBuf;

use windows::Win32::Graphics::{Direct2D::ID2D1Bitmap, Imaging::IWICBitmapSource};
use zsdesk_core::model::explorer::WallpaperPreferences;
use zsdesk_windows::gfx::{Canvas, Gfx, RectF};

/// Everything the decoded picture depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BaseKey {
    path: PathBuf,
    edge: i32,
}

/// Everything the per-window render depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WallpaperKey {
    path: PathBuf,
    width: i32,
    height: i32,
    blur: u8,
    dim: u8,
}

pub(crate) struct WallpaperLayer {
    /// The decoded picture, shared by every window size.
    base: Option<(BaseKey, IWICBitmapSource)>,
    /// What the window-size render was made from.
    key: Option<WallpaperKey>,
    /// Source pixels kept so a render-target reset does not render again.
    source: Option<IWICBitmapSource>,
    bitmap: Option<(u64, ID2D1Bitmap)>,
    /// Mean luminance of the picture currently on screen, 0..=1.
    mean: f32,
    /// A path that failed to decode is remembered so the attempt runs once.
    failed: Option<BaseKey>,
}

impl Default for WallpaperLayer {
    fn default() -> Self { Self { base: None, key: None, source: None, bitmap: None, mean: 0.0, failed: None } }
}

impl WallpaperLayer {
    /// Returns the picture for this frame, decoding it only when the picture
    /// itself changed and deriving it only when the window size changed.
    pub(crate) fn prepare(&mut self, gfx: &Gfx, width: i32, height: i32, prefs: &WallpaperPreferences) -> Option<(ID2D1Bitmap, f32)> {
        let opacity = prefs.opacity as f32 / 100.0;
        let path = prefs.path.as_ref()?;
        if opacity <= 0.0 || width <= 0 || height <= 0 { return None; }
        // The blur runs on the CPU, so the picture is rendered below the window
        // size and stretched when drawn: the cap bounds that work.
        let (width, height) = decode_size(width, height);
        let base_key = BaseKey { path: path.clone(), edge: BASE_EDGE };
        if self.failed.as_ref() == Some(&base_key) { return None; }
        if self.base.as_ref().map(|(key, _)| key) != Some(&base_key) {
            self.base = None;
            self.key = None;
            self.source = None;
            self.bitmap = None;
            match gfx.wallpaper_base(path, base_key.edge) {
                Some(source) => self.base = Some((base_key, source)),
                None => { self.failed = Some(base_key); return None; }
            }
        }
        let key = WallpaperKey { path: path.clone(), width, height, blur: prefs.blur.min(100), dim: prefs.dim.min(100) };
        if self.key.as_ref() != Some(&key) {
            // A window edge being dragged changes the size on every step, and
            // rendering costs milliseconds. The picture on hand is stretched
            // instead and rendered at the exact size once the drag stops, which
            // `settle` does.
            if !self.stretchable(&key) && !self.render(gfx, key) {
                return None;
            }
        }
        let generation = gfx.generation();
        if self.bitmap.as_ref().is_none_or(|(cached, _)| *cached != generation) {
            self.bitmap = self.source.as_ref().and_then(|source| gfx.upload(source)).map(|bitmap| (generation, bitmap));
        }
        self.bitmap.as_ref().map(|(_, bitmap)| (bitmap.clone(), opacity))
    }

    /// True when the picture on hand differs from `key` only in size, so it can
    /// be stretched for a moment instead of rendered again.
    fn stretchable(&self, key: &WallpaperKey) -> bool {
        let Some(current) = &self.key else { return false };
        if self.bitmap.is_none() || current.path != key.path || current.blur != key.blur || current.dim != key.dim { return false }
        // A small mismatch only stretches the picture a little; a large one
        // would bend it visibly, so it is rendered instead.
        let close = |want: i32, have: i32| (want - have).abs() * 12 <= have.abs().max(1);
        close(key.width, current.width) && close(key.height, current.height)
    }

    /// Renders `key` now and makes it the picture on screen.
    fn render(&mut self, gfx: &Gfx, key: WallpaperKey) -> bool {
        self.key = Some(key.clone());
        self.source = None;
        self.bitmap = None;
        let rendered = self.base.as_ref()
            .and_then(|(_, base)| gfx.wallpaper_render(base, key.width, key.height, key.blur, key.dim));
        // A picture that decodes but cannot be scaled shows nothing; the key is
        // kept so the attempt does not repeat on every frame.
        let Some((source, mean)) = rendered else { return false };
        self.source = Some(source);
        self.mean = mean;
        true
    }

    /// Renders the picture for the size the window has now. A drag leaves a
    /// stretched picture behind; this replaces it once the window stopped
    /// changing size. Returns true when the picture was re-rendered.
    pub(crate) fn settle(&mut self, gfx: &Gfx, width: i32, height: i32, prefs: &WallpaperPreferences) -> bool {
        let (Some(path), false) = (prefs.path.as_ref(), prefs.opacity == 0 || width <= 0 || height <= 0) else { return false };
        let (width, height) = decode_size(width, height);
        let key = WallpaperKey { path: path.clone(), width, height, blur: prefs.blur.min(100), dim: prefs.dim.min(100) };
        if self.key.as_ref() == Some(&key) || self.bitmap.is_none() { return false; }
        self.render(gfx, key)
    }

    /// Mean luminance of the picture currently on screen, 0..=1.
    pub(crate) fn mean(&self) -> f32 { self.mean }

    /// Forgets the decoded picture, e.g. after the user edited the preference.
    pub(crate) fn invalidate(&mut self) {
        self.base = None;
        self.key = None;
        self.source = None;
        self.bitmap = None;
        self.mean = 0.0;
        self.failed = None;
    }
}

/// Longest edge the picture is decoded at. A window is rarely larger, so the
/// base serves every size without being upscaled.
const BASE_EDGE: i32 = 1920;
/// Largest edge the picture is rendered at, and the step its size is rounded to.
const MAX_DECODE: i32 = 1600;
const DECODE_STEP: i32 = 16;

/// Size the picture is rendered at. Rounding keeps the cache key stable while a
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
