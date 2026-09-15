//! Bounded, non-blocking icon loading and texture caching for egui.
//!
//! Keep one [`IconCache`] alongside the UI state and call [`IconCache::show_url`]
//! or [`IconCache::show_bytes`] on each frame that draws an icon. Calls poll finished
//! work, admit a missing key when capacity permits, and paint either its texture or
//! a quiet placeholder. This module does not install egui image loaders.
//!
//! Each admitted icon gets a short-lived worker thread. Downloading and decoding
//! happen there; the UI-owning thread alone drains completions and creates
//! [`egui::TextureHandle`]s, matching egui's context/texture lifecycle. A bounded
//! synchronous channel carries at most four queued completions, and `pending` limits
//! admission to four live jobs. Worker-owned sender/context clones keep those values
//! alive only for the job. Dropping the cache drops its receiver and texture handles
//! immediately, detaches any workers instead of joining them, and causes their later
//! sends to fail harmlessly.
//!
//! HTTPS (including redirects) is required, with a ten-second total request timeout
//! and a 2 MiB encoded size cap. PNG, JPEG, and WebP inputs are limited to 2048 per
//! axis, 2048² pixels, and a 32 MiB decoder allocation budget; animations use the
//! decoder's first frame. Accepted artwork is reduced to a 128-pixel thumbnail before
//! upload, bounding 128 RGBA textures to approximately 8 MiB plus cache overhead.
//!
//! The 128-entry least-recently-used cache includes failures, preventing an invalid
//! icon from being retried every frame. Eviction allows a later retry, but pending
//! entries are pinned so a worker completion always has a stable key. URL and byte
//! keys occupy separate namespaces. A byte key must identify immutable content—use
//! a new key when bytes change. `None` schedules nothing and only draws a placeholder.

use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

// Independent limits bound network/memory input, decoder work, workers, and textures.
const MAX_ENCODED: usize = 2 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2048;
const MAX_PIXELS: u64 = 2048 * 2048;
const MAX_JOBS: usize = 4;
const MAX_ENTRIES: usize = 128;

/// Cache identity; variants prevent a caller byte key from aliasing a URL.
#[derive(Clone, Hash, PartialEq, Eq)]
enum Key {
    Url(String),
    Bytes(String),
}

/// Lifecycle of an admitted key. Failed entries are cached intentionally.
enum State {
    /// A worker owns the corresponding completion sender clone.
    Pending,
    /// Loading, validation, decoding, or worker creation failed.
    Failed,
    /// GPU texture created and owned on the UI side.
    Ready(egui::TextureHandle),
}

struct Entry {
    state: State,
    /// Logical paint clock used for least-recently-used eviction.
    last_used: u64,
}

/// Worker-to-UI message; `None` deliberately collapses all load failures.
type Completion = (Key, Option<egui::ColorImage>);

/// UI-owned, bounded icon cache and completion endpoint.
///
/// Construct with [`IconCache::default`] and reuse it across frames; creating one
/// per frame discards textures/failure history and resets the worker bound. The type
/// owns the only receiver and mutates entries through `&mut self`, so polling,
/// admission, LRU updates, and texture creation remain serialized on its owner.
pub struct IconCache {
    entries: HashMap<Key, Entry>,
    /// Root sender cloned into each worker; retained so the channel stays connected.
    sender: SyncSender<Completion>,
    /// Sole completion consumer, polled without blocking from drawing calls.
    receiver: Receiver<Completion>,
    /// Number of successfully spawned workers whose completion is not yet drained.
    pending: usize,
    /// Saturating logical time; ties are acceptable for approximate LRU eviction.
    clock: u64,
}

impl Default for IconCache {
    fn default() -> Self {
        // Capacity matches the job bound, so all workers can publish one result even
        // when the UI does not poll again until a later frame.
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
    /// Draws a square URL icon or a quiet placeholder while absent/pending/failed.
    ///
    /// Only HTTPS URLs are fetched. If worker or cache capacity is currently pinned,
    /// the key is not inserted, allowing the caller's next frame to retry admission.
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

    /// Draws encoded bytes under a stable, caller-provided content key.
    ///
    /// Oversized buffers become cached failures without being copied; accepted bytes
    /// are copied once to satisfy the worker closure's `'static` lifetime after this
    /// borrowed call returns. Missing bytes neither create nor invalidate entries.
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

    /// Reserves a pinned pending entry when both worker and cache bounds allow it.
    ///
    /// At capacity, the least recently painted non-pending entry is removed. Pending
    /// entries cannot be evicted because their workers still hold keys and will send
    /// completions; if every entry is pending, admission waits for a later frame.
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

    /// Spawns one loader/decoder and arranges a repaint after completion.
    ///
    /// `FnOnce + Send + 'static` transfers owned job input to a worker that may outlive
    /// this call. Panic capture converts codec panics into ordinary cached failures;
    /// failure to spawn transitions the reserved entry immediately and does not
    /// increment `pending`.
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
                // Convert every worker outcome, including a codec panic, into one completion.
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

    /// Drains available worker messages without blocking the UI thread.
    ///
    /// Texture upload happens here rather than on workers. Each received message
    /// balances one successful spawn, while a missing entry is tolerated defensively.
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

    /// Updates recency and paints a fitted texture or placeholder in a square slot.
    ///
    /// Non-finite/negative requested sizes collapse safely to zero. Aspect ratio is
    /// preserved, and periodic repaint requests ensure progress is observed even if
    /// a platform does not promptly act on the worker's cross-thread repaint signal.
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

/// Downloads a bounded HTTPS response, collapsing network/status failures to `None`.
///
/// The client also enforces HTTPS after redirects. `Content-Length` permits an early
/// rejection, but the body is independently read through a `limit + 1` sentinel so
/// absent or dishonest headers cannot bypass the encoded-size bound.
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
    let mut bytes = Vec::new();
    response
        .take(MAX_ENCODED as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_ENCODED).then_some(bytes)
}

/// Validates, decodes, and thumbnails untrusted encoded image bytes.
///
/// Format allowlisting happens before decoder construction. Decoder allocation and
/// dimensions are bounded both through `image::Limits` and explicit checked-width
/// arithmetic, then the result is converted to egui's unmultiplied RGBA representation.
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
