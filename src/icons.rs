//! Small, non-blocking icon cache for egui. Own one `IconCache` alongside UI state
//! and call `show_url` or `show_bytes` each frame. This module needs to be wired by
//! its caller; it does not install egui image loaders.
//!
//! At most four jobs run concurrently. Downloads and decoding happen on worker
//! threads; only the UI thread uploads textures. HTTPS (including redirects) is
//! required, with a ten-second request timeout and a 2 MiB encoded size cap.
//! Images are limited to 2048 per axis, 2048² pixels, and a 32 MiB decoder budget.
//! PNG, JPEG, and WebP are enabled.
//! Animations use the first frame.
//!
//! The 128-entry LRU includes failures, so bad icons do not retry or log every
//! frame. Eviction permits a later retry; pending entries are never evicted.
//! Byte keys must identify immutable content (change the key when bytes change).
//! URL and byte keys have separate namespaces. `None` only draws a placeholder.
//! Dropping the cache detaches at most four bounded-time jobs; it never waits for
//! network I/O. Decoded artwork is reduced to 128-pixel thumbnails before upload,
//! bounding the 128-entry RGBA texture cache to approximately 8 MiB.

use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

const MAX_ENCODED: usize = 2 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2048;
const MAX_PIXELS: u64 = 2048 * 2048;
const MAX_JOBS: usize = 4;
const MAX_ENTRIES: usize = 128;

#[derive(Clone, Hash, PartialEq, Eq)]
enum Key {
    Url(String),
    Bytes(String),
}

enum State {
    Pending,
    Failed,
    Ready(egui::TextureHandle),
}

struct Entry {
    state: State,
    last_used: u64,
}

type Completion = (Key, Option<egui::ColorImage>);

/// UI-owned, bounded cache. Construct with `IconCache::default()` and reuse it
/// across frames; creating one per frame defeats caching and job bounds.
pub struct IconCache {
    entries: HashMap<Key, Entry>,
    sender: SyncSender<Completion>,
    receiver: Receiver<Completion>,
    pending: usize,
    clock: u64,
}

impl Default for IconCache {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_JOBS);
        Self {
            entries: HashMap::new(),
            sender,
            receiver,
            pending: 0,
            clock: 0,
        }
    }
}

impl IconCache {
    /// Draw a square icon, or a quiet placeholder while absent, queued, or failed.
    /// Only HTTPS URLs are fetched. Saturated callers retry admission next frame.
    pub fn show_url(&mut self, ui: &mut egui::Ui, url: Option<&str>, size: f32) {
        self.poll(ui.ctx());
        let key = url.map(|url| Key::Url(url.to_owned()));
        if let Some(key) = &key {
            if self.admit(key) {
                let url = url.expect("a URL key has a URL").to_owned();
                self.start(ui.ctx(), key.clone(), move || download(&url));
            }
        }
        self.paint(ui, key.as_ref(), size);
    }

    /// Draw encoded bytes under a stable, caller-provided content key.
    /// Oversized buffers fail without being copied; accepted buffers are copied
    /// once for the worker. Missing bytes neither create nor invalidate entries.
    pub fn show_bytes(&mut self, ui: &mut egui::Ui, key: &str, bytes: Option<&[u8]>, size: f32) {
        self.poll(ui.ctx());
        let key = bytes.map(|_| Key::Bytes(key.to_owned()));
        if let (Some(key), Some(bytes)) = (&key, bytes) {
            if self.admit(key) {
                if bytes.len() > MAX_ENCODED {
                    self.entries.get_mut(key).unwrap().state = State::Failed;
                } else {
                    let bytes = bytes.to_vec();
                    self.start(ui.ctx(), key.clone(), move || Some(bytes));
                }
            }
        }
        self.paint(ui, key.as_ref(), size);
    }

    fn admit(&mut self, key: &Key) -> bool {
        if self.entries.contains_key(key) || self.pending >= MAX_JOBS {
            return false;
        }
        if self.entries.len() >= MAX_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| !matches!(entry.state, State::Pending))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            } else {
                return false;
            }
        }
        self.entries.insert(
            key.clone(),
            Entry {
                state: State::Pending,
                last_used: self.clock,
            },
        );
        true
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        key: Key,
        load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ) {
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let worker_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("icon-loader".into())
            .spawn(move || {
                // Always complete the pending entry, even if a codec panics.
                let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    load().and_then(|bytes| decode(&bytes))
                }))
                .ok()
                .flatten();
                let _ = sender.send((worker_key, image));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.pending += 1;
        } else {
            self.entries.get_mut(&key).unwrap().state = State::Failed;
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, image)) = self.receiver.try_recv() {
            self.pending -= 1;
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.state = match image {
                    Some(image) => State::Ready(ctx.load_texture(
                        "cached-icon",
                        image,
                        egui::TextureOptions::LINEAR,
                    )),
                    None => State::Failed,
                };
            }
        }
    }

    fn paint(&mut self, ui: &mut egui::Ui, key: Option<&Key>, size: f32) {
        self.clock = self.clock.saturating_add(1);
        let size = if size.is_finite() { size.max(0.0) } else { 0.0 };
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
        let texture = key
            .and_then(|key| self.entries.get_mut(key))
            .and_then(|entry| {
                entry.last_used = self.clock;
                match &entry.state {
                    State::Ready(texture) => Some(texture),
                    _ => None,
                }
            });
        if let Some(texture) = texture {
            let dimensions = texture.size_vec2();
            let fitted = dimensions * (size / dimensions.max_elem());
            ui.painter().image(
                texture.id(),
                egui::Rect::from_center_size(rect.center(), fitted),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            ui.painter()
                .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
            ui.painter()
                .circle_filled(rect.center(), size * 0.12, ui.visuals().weak_text_color());
        }
        if self.pending > 0 {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn download(url: &str) -> Option<Vec<u8>> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .ok()?;
    let response = client.get(url).send().ok()?.error_for_status().ok()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ENCODED as u64)
    {
        return None;
    }
    // The extra byte detects over-limit chunked bodies without trusting headers.
    let mut bytes = Vec::new();
    response
        .take(MAX_ENCODED as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_ENCODED).then_some(bytes)
}

fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    if bytes.is_empty() || bytes.len() > MAX_ENCODED {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().ok()?;
    use image::ImageDecoder;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return None;
    }

    let decoded = image::DynamicImage::from_decoder(decoder).ok()?;
    let thumbnail = if width > 128 || height > 128 {
        decoded.thumbnail(128, 128)
    } else {
        decoded
    };
    let rgba = thumbnail.into_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn decodes_png_and_rejects_invalid_or_oversized_input() {
        let image = decode(&png(2, 3)).unwrap();
        assert_eq!(image.size, [2, 3]);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(10, 20, 30));
        assert!(decode(b"invalid").is_none());
        assert!(decode(&vec![0; MAX_ENCODED + 1]).is_none());
        assert!(decode(&png(MAX_DIMENSION + 1, 1)).is_none());
        assert!(decode(&png(1, MAX_DIMENSION + 1)).is_none());
        assert_eq!(decode(&png(512, 512)).unwrap().size, [128, 128]);
    }

    #[test]
    fn rejects_non_https_without_network() {
        assert!(download("http://example.com/icon.png").is_none());
        assert!(download("file:///icon.png").is_none());
    }

    #[test]
    fn bounds_cache_and_keeps_pending_and_failed_entries() {
        let mut cache = IconCache::default();
        let pinned = Key::Bytes("pending".into());
        assert!(cache.admit(&pinned));
        assert!(!cache.admit(&pinned));
        for i in 0..MAX_ENTRIES * 2 {
            let key = Key::Bytes(i.to_string());
            assert!(cache.admit(&key));
            cache.entries.get_mut(&key).unwrap().state = State::Failed;
            assert!(!cache.admit(&key));
            assert!(cache.entries.len() <= MAX_ENTRIES);
        }
        assert!(cache.entries.contains_key(&pinned));
        cache.pending = MAX_JOBS;
        assert!(!cache.admit(&Key::Url("https://example.com/new.png".into())));
    }
}
