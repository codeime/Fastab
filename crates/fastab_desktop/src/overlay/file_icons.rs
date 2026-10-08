//! Own dynamically generated file icons without GPUI's process-wide asset cache.
//!
//! Both indexes are bounded. PNG content, rather than its filesystem path, owns
//! the decoded image so aliases cannot evict one another's atlas entry. The
//! shared idle task also retires the hidden native window, including its atlas.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fastab_gpui::{OverlayHandle, OverlayState};
use gpui::{App, Context, Entity, Image, ImageId, RenderImage, Task, WeakEntity};

const CAPACITY: usize = 64;
const MAX_UNCACHED_PER_RESULT: usize = 8;
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const ICON_PIXELS: u32 = 64;
const MAX_SOURCE_SIDE: u32 = 1024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

struct CachedIcon {
    image: Arc<RenderImage>,
    last_used: u64,
    cached: bool,
}

#[derive(Default)]
pub(super) struct FileIconCache {
    paths: HashMap<PathBuf, (u64, u64)>,
    images: HashMap<u64, CachedIcon>,
    batch: HashSet<u64>,
    clock: u64,
    idle_generation: u64,
    idle_task: Option<Task<()>>,
}

impl FileIconCache {
    pub(super) fn begin_batch(&mut self) {
        self.cancel_idle();
        self.batch.clear();
    }

    pub(super) fn cancel_idle(&mut self) {
        self.idle_generation = self.idle_generation.wrapping_add(1);
        self.idle_task.take();
    }

    pub(super) fn lookup(
        &mut self,
        cwd: &str,
        name: &str,
        kind: &str,
        uncached: &mut usize,
    ) -> Option<Arc<RenderImage>> {
        if kind != "file" && kind != "folder" {
            return None;
        }
        let path = PathBuf::from(if cwd.is_empty() { "." } else { cwd }).join(name.trim_end_matches('/'));
        if let Some(&(content, _)) = self.paths.get(&path) {
            return self.touch(path, content);
        }
        if *uncached >= MAX_UNCACHED_PER_RESULT {
            return None;
        }
        *uncached += 1;
        #[cfg(target_os = "macos")]
        {
            // AppKit can return a large source representation even after its
            // point size was changed. Bound decoding and the retained pixels.
            let bytes = unsafe { macos_utils::image::png_for_path(&path) }?;
            self.insert_png(path, bytes)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            None
        }
    }

    fn insert_png(&mut self, path: PathBuf, bytes: Vec<u8>) -> Option<Arc<RenderImage>> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return None;
        }
        let source = Image::from_bytes(gpui::ImageFormat::Png, bytes);
        let content = source.id();
        if !self.images.get(&content).is_some_and(|entry| entry.cached) {
            self.reserve_content()?;
            if let Some(entry) = self.images.get_mut(&content) {
                // The same PNG may still be awaiting scene retirement.
                entry.cached = true;
            } else {
                let image = decode_icon(&source.bytes)?;
                self.images.insert(
                    content,
                    CachedIcon {
                        image,
                        last_used: 0,
                        cached: true,
                    },
                );
            }
        }
        self.touch(path, content)
    }

    fn reserve_content(&mut self) -> Option<()> {
        if self.images.values().filter(|entry| entry.cached).count() < CAPACITY {
            return Some(());
        }
        // A large result must not evict icons already assigned to its earlier
        // rows. Additional unique icons use the normal bundled fallback.
        let victim = self
            .images
            .iter()
            .filter(|(content, entry)| entry.cached && !self.batch.contains(content))
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(&content, _)| content)?;
        self.images.get_mut(&victim)?.cached = false;
        self.paths.retain(|_, (content, _)| *content != victim);
        Some(())
    }

    fn touch(&mut self, path: PathBuf, content: u64) -> Option<Arc<RenderImage>> {
        self.clock = self.clock.saturating_add(1);
        let entry = self.images.get_mut(&content)?;
        entry.last_used = self.clock;
        self.batch.insert(content);
        let image = entry.image.clone();
        if !self.paths.contains_key(&path) && self.paths.len() >= CAPACITY {
            if let Some(old) = self
                .paths
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(path, _)| path.clone())
            {
                self.paths.remove(&old);
            }
        }
        self.paths.insert(path, (content, self.clock));
        Some(image)
    }

    fn take_retired(&mut self, used: &HashSet<ImageId>) -> Vec<Arc<RenderImage>> {
        let retired = self
            .images
            .iter()
            .filter(|(_, entry)| !entry.cached && !used.contains(&entry.image.id))
            .map(|(&content, _)| content)
            .collect::<Vec<_>>();
        retired
            .into_iter()
            .filter_map(|content| self.images.remove(&content).map(|entry| entry.image))
            .collect()
    }

    pub(super) fn finish_batch(
        &mut self,
        state: &Entity<OverlayState>,
        slot: &Mutex<Option<OverlayHandle>>,
        cx: &mut App,
    ) {
        let used = row_images(state, cx);
        let retired = self.take_retired(&used);
        release_atlas_images(retired, slot, cx);
        self.batch.clear();
    }

    pub(super) fn schedule_idle(
        &mut self,
        state: WeakEntity<OverlayState>,
        slot: Arc<Mutex<Option<OverlayHandle>>>,
        cx: &mut Context<'_, Self>,
    ) {
        // Even an overlay with only text and bundled icons owns a renderer.
        // Every hide starts a fresh grace period, including after a brief show.
        self.cancel_idle();
        let generation = self.idle_generation;
        self.idle_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(IDLE_TIMEOUT).await;
            let _ = this.update(cx, |cache, cx| {
                if cache.idle_generation != generation {
                    return;
                }
                let Some(state) = state.upgrade() else { return };
                if state.read(cx).visible {
                    return;
                }
                let handle = *slot.lock().unwrap_or_else(|err| err.into_inner());
                if let Some(handle) = handle {
                    // Invalidate queued AppKit positioning before dropping the
                    // native window those requests address. Window destruction
                    // releases its scenes, renderer and atlas together.
                    let _ = fastab_gpui::park_overlay_handle(&handle, cx);
                    let _ = gpui::AnyWindowHandle::from(handle).update(cx, |_, window, _| {
                        window.remove_window();
                    });
                    super::clear_overlay_handle_if(&slot, handle);
                }
                cache.paths.clear();
                for entry in cache.images.values_mut() {
                    entry.cached = false;
                }
                // Tab recreates a window from this same state, so retain just
                // the small decoded images referenced by the kept rows.
                drop(cache.take_retired(&row_images(&state, cx)));
                cache.batch.clear();
            });
        }));
    }
}

fn decode_icon(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let source = reader.decode().ok()?;
    let source = if source.width() > ICON_PIXELS || source.height() > ICON_PIXELS {
        source.thumbnail(ICON_PIXELS, ICON_PIXELS)
    } else {
        source
    };
    let mut pixels = source.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
}

fn row_images(state: &Entity<OverlayState>, cx: &App) -> HashSet<ImageId> {
    state
        .read(cx)
        .items
        .iter()
        .filter_map(|item| item.icon_png.as_ref().map(|image| image.id))
        .collect()
}

fn release_atlas_images(images: Vec<Arc<RenderImage>>, slot: &Mutex<Option<OverlayHandle>>, cx: &mut App) {
    if images.is_empty() {
        return;
    }
    let handle = *slot.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(handle) = handle {
        // A typed WindowHandle::update would lease SuggestionList while draw
        // needs to update that same root entity. Only borrow the window here.
        let _ = gpui::AnyWindowHandle::from(handle).update(cx, |_, window, cx| {
            // This runs from desktop events, after the rows'
            // entity update has returned, never from a render/paint callback.
            // Rebuild the CPU scene before removing tiles it previously used.
            // draw does not present; the hidden root is an empty div. Already
            // committed Metal commands retain their own texture references.
            window.refresh();
            window.draw(cx).clear();
            for image in &images {
                let _ = window.drop_image(image.clone());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    fn png(value: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([value, 31, 63, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[gpui::test]
    fn idle_grace_restarts_and_preserves_only_images_in_kept_rows(cx: &mut gpui::TestAppContext) {
        let slot = Arc::new(Mutex::new(None));
        let (cache, state, unused, kept) = cx.update(|cx| {
            let state = cx.new(|_| OverlayState::new());
            let cache = cx.new(|_| FileIconCache::default());
            let (unused, kept) = cache.update(cx, |cache, cx| {
                let unused = cache.insert_png(PathBuf::from("unused"), png(1)).unwrap();
                let kept = cache.insert_png(PathBuf::from("kept"), png(2)).unwrap();
                state.update(cx, |state, _| {
                    state.items.push(fastab_gpui::SuggestionItem {
                        name: "kept".into(),
                        icon_png: Some(kept.clone()),
                        ..Default::default()
                    });
                });
                cache.schedule_idle(state.downgrade(), slot.clone(), cx);
                (Arc::downgrade(&unused), Arc::downgrade(&kept))
            });
            (cache, state, unused, kept)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(6));
        // Showing cancels the old idle period. Hiding again starts a new one.
        cx.update(|cx| cache.update(cx, |cache, _| cache.cancel_idle()));
        cx.executor().advance_clock(Duration::from_secs(6));
        cx.run_until_parked();
        assert!(unused.upgrade().is_some());
        cx.update(|cx| {
            cache.update(cx, |cache, cx| {
                cache.schedule_idle(state.downgrade(), slot.clone(), cx);
            });
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(9));
        cx.run_until_parked();
        assert!(unused.upgrade().is_some(), "a new hide gets the full ten seconds");
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(unused.upgrade().is_none());
        assert!(kept.upgrade().is_some(), "Tab still owns the kept row's decoded image");
        cx.update(|cx| {
            assert!(cache.read(cx).paths.is_empty());
            state.update(cx, |state, _| state.items.clear());
            cache.update(cx, |cache, cx| cache.finish_batch(&state, &slot, cx));
        });
        assert!(kept.upgrade().is_none());
    }

    #[test]
    fn unique_pngs_retire_decoded_images_instead_of_accumulating() {
        let mut cache = FileIconCache::default();
        let mut weak = Vec::new();
        for index in 0..96u8 {
            cache.begin_batch();
            let image = cache.insert_png(PathBuf::from(index.to_string()), png(index)).unwrap();
            weak.push(Arc::downgrade(&image));
            drop(image);
            drop(cache.take_retired(&HashSet::new()));
            assert!(cache.images.len() <= CAPACITY);
            assert!(cache.paths.len() <= CAPACITY);
        }
        assert_eq!(weak.iter().filter(|image| image.upgrade().is_some()).count(), CAPACITY);
        drop(cache);
        assert!(weak.iter().all(|image| image.upgrade().is_none()));
    }

    #[test]
    fn aliases_share_content_and_current_batch_cannot_evict_itself() {
        let mut cache = FileIconCache::default();
        cache.begin_batch();
        let first = cache.insert_png(PathBuf::from("a"), png(0)).unwrap();
        for index in 0..96u8 {
            let alias = cache
                .insert_png(PathBuf::from(format!("alias-{index}")), png(0))
                .unwrap();
            assert!(Arc::ptr_eq(&first, &alias));
        }
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.paths.len(), CAPACITY);
        for index in 1..CAPACITY as u8 {
            assert!(cache.insert_png(PathBuf::from(index.to_string()), png(index)).is_some());
        }
        assert!(cache.insert_png(PathBuf::from("overflow"), png(255)).is_none());
        assert!(cache.take_retired(&HashSet::new()).is_empty());
        assert_eq!(cache.images.len(), CAPACITY);
    }

    #[test]
    fn rows_pin_retired_images_until_their_scene_can_be_replaced() {
        let mut cache = FileIconCache::default();
        let image = cache.insert_png(PathBuf::from("visible"), png(0)).unwrap();
        let weak = Arc::downgrade(&image);
        let used = HashSet::from([image.id]);
        cache.paths.clear();
        cache.images.values_mut().for_each(|entry| entry.cached = false);
        drop(image);
        assert!(cache.take_retired(&used).is_empty());
        assert!(weak.upgrade().is_some());
        let retiring = cache.take_retired(&HashSet::new());
        assert!(
            weak.upgrade().is_some(),
            "the retirement batch owns the image until atlas cleanup"
        );
        drop(retiring);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn large_png_is_reduced_to_icon_pixels_and_oversize_input_is_rejected() {
        let mut cache = FileIconCache::default();
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(768, 512, image::Rgba([7, 31, 63, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let image = cache.insert_png(PathBuf::from("large"), bytes.into_inner()).unwrap();
        let size = image.size(0);
        assert_eq!(size.width.0, ICON_PIXELS as i32);
        assert!(size.height.0 > 0 && size.height.0 <= ICON_PIXELS as i32);
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[63, 31, 7, 255]);
        assert!(image.as_bytes(0).unwrap().len() <= (ICON_PIXELS * ICON_PIXELS * 4) as usize);

        let mut oversized = Cursor::new(Vec::new());
        image::RgbaImage::new(MAX_SOURCE_SIDE + 1, 1)
            .write_to(&mut oversized, image::ImageFormat::Png)
            .unwrap();
        assert!(
            cache
                .insert_png(PathBuf::from("oversized"), oversized.into_inner())
                .is_none()
        );
        assert!(
            cache
                .insert_png(PathBuf::from("too-many-bytes"), vec![0; MAX_SOURCE_BYTES + 1])
                .is_none()
        );
    }
}
