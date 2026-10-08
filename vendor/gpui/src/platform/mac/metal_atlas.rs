use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, PlatformAtlas, Point, Size,
    platform::AtlasTextureList,
};
use anyhow::{Context as _, Result};
use collections::FxHashMap;
use derive_more::{Deref, DerefMut};
use etagere::BucketedAtlasAllocator;
use metal::Device;
use parking_lot::Mutex;
use std::borrow::Cow;

pub(crate) struct MetalAtlas(Mutex<MetalAtlasState>);

impl MetalAtlas {
    pub(crate) fn new(device: Device) -> Self {
        MetalAtlas(Mutex::new(MetalAtlasState {
            device: AssertSend(device),
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            tiles_by_key: Default::default(),
        }))
    }

    pub(crate) fn metal_texture(&self, id: AtlasTextureId) -> metal::Texture {
        self.0.lock().texture(id).metal_texture.clone()
    }
}

struct MetalAtlasState {
    device: AssertSend<Device>,
    monochrome_textures: AtlasTextureList<MetalAtlasTexture>,
    polychrome_textures: AtlasTextureList<MetalAtlasTexture>,
    tiles_by_key: FxHashMap<AtlasKey, AtlasTile>,
}

impl PlatformAtlas for MetalAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        let mut lock = self.0.lock();
        if let Some(tile) = lock.tiles_by_key.get(key) {
            Ok(Some(tile.clone()))
        } else {
            let Some((size, bytes)) = build()? else {
                return Ok(None);
            };
            let tile = lock.allocate(size, key.texture_kind()).context("failed to allocate")?;
            let texture = lock.texture(tile.texture_id);
            texture.upload(tile.bounds, &bytes);
            lock.tiles_by_key.insert(key.clone(), tile.clone());
            Ok(Some(tile))
        }
    }

    fn remove(&self, key: &AtlasKey) {
        let mut lock = self.0.lock();
        // Each key owns exactly one texture reference. Remove the mapping even
        // when other tiles still use the texture, so repeated removal is a no-op
        // and a subsequent lookup rebuilds the tile.
        let Some(id) = lock.tiles_by_key.remove(key).map(|v| v.texture_id) else {
            return;
        };

        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut lock.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut lock.polychrome_textures,
        };

        let Some(texture_slot) = textures
            .textures
            .iter_mut()
            .find(|texture| texture.as_ref().is_some_and(|v| v.id == id))
        else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.decrement_ref_count();

            if texture.is_unreferenced() {
                textures.free_list.push(id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl MetalAtlasState {
    fn allocate(&mut self, size: Size<DevicePixels>, texture_kind: AtlasTextureKind) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };

            if let Some(tile) = textures.iter_mut().rev().find_map(|texture| texture.allocate(size)) {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind);
        texture.allocate(size)
    }

    fn push_texture(&mut self, min_size: Size<DevicePixels>, kind: AtlasTextureKind) -> &mut MetalAtlasTexture {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size on all modern Apple GPUs. Anything bigger than that crashes in validateWithDevice.
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE);
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.into());
        texture_descriptor.set_height(size.height.into());
        let pixel_format;
        let usage;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = metal::MTLPixelFormat::A8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            },
            AtlasTextureKind::Polychrome => {
                pixel_format = metal::MTLPixelFormat::BGRA8Unorm;
                usage = metal::MTLTextureUsage::ShaderRead;
            },
        }
        texture_descriptor.set_pixel_format(pixel_format);
        texture_descriptor.set_usage(usage);
        let metal_texture = self.device.new_texture(&texture_descriptor);

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };

        let index = texture_list.free_list.pop();

        let atlas_texture = MetalAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            allocator: etagere::BucketedAtlasAllocator::new(size.into()),
            metal_texture: AssertSend(metal_texture),
            live_atlas_keys: 0,
        };

        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix)
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut()
        }
        .unwrap()
        .as_mut()
        .unwrap()
    }

    fn texture(&self, id: AtlasTextureId) -> &MetalAtlasTexture {
        let textures = match id.kind {
            crate::AtlasTextureKind::Monochrome => &self.monochrome_textures,
            crate::AtlasTextureKind::Polychrome => &self.polychrome_textures,
        };
        textures[id.index as usize].as_ref().unwrap()
    }
}

struct MetalAtlasTexture {
    id: AtlasTextureId,
    allocator: BucketedAtlasAllocator,
    metal_texture: AssertSend<metal::Texture>,
    live_atlas_keys: u32,
}

impl MetalAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(size.into())?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: allocation.rectangle.min.into(),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(&self, bounds: Bounds<DevicePixels>, bytes: &[u8]) {
        let region = metal::MTLRegion::new_2d(
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        );
        self.metal_texture.replace_region(
            region,
            0,
            bytes.as_ptr() as *const _,
            bounds.size.width.to_bytes(self.bytes_per_pixel()) as u64,
        );
    }

    fn bytes_per_pixel(&self) -> u8 {
        use metal::MTLPixelFormat::*;
        match self.metal_texture.pixel_format() {
            A8Unorm | R8Unorm => 1,
            RGBA8Unorm | BGRA8Unorm => 4,
            _ => unimplemented!(),
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

impl From<Size<DevicePixels>> for etagere::Size {
    fn from(size: Size<DevicePixels>) -> Self {
        etagere::Size::new(size.width.into(), size.height.into())
    }
}

impl From<etagere::Point> for Point<DevicePixels> {
    fn from(value: etagere::Point) -> Self {
        Point {
            x: DevicePixels::from(value.x),
            y: DevicePixels::from(value.y),
        }
    }
}

impl From<etagere::Size> for Size<DevicePixels> {
    fn from(size: etagere::Size) -> Self {
        Size {
            width: DevicePixels::from(size.width),
            height: DevicePixels::from(size.height),
        }
    }
}

impl From<etagere::Rectangle> for Bounds<DevicePixels> {
    fn from(rectangle: etagere::Rectangle) -> Self {
        Bounds {
            origin: rectangle.min.into(),
            size: rectangle.size().into(),
        }
    }
}

#[derive(Deref, DerefMut)]
struct AssertSend<T>(T);

unsafe impl<T> Send for AssertSend<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImageId, RenderImageParams};

    fn key(index: usize) -> AtlasKey {
        RenderImageParams {
            image_id: ImageId(index),
            frame_index: 0,
        }
        .into()
    }

    fn insert(atlas: &MetalAtlas, key: &AtlasKey, value: u8) -> AtlasTile {
        atlas
            .get_or_insert_with(key, &mut || {
                Ok(Some((
                    Size {
                        width: DevicePixels(2),
                        height: DevicePixels(2),
                    },
                    Cow::Owned(vec![value; 16]),
                )))
            })
            .unwrap()
            .unwrap()
    }

    fn pixels(texture: &metal::TextureRef, tile: &AtlasTile) -> Vec<u8> {
        let mut bytes = vec![0; 16];
        texture.get_bytes(
            bytes.as_mut_ptr().cast(),
            8,
            metal::MTLRegion::new_2d(tile.bounds.origin.x.0 as u64, tile.bounds.origin.y.0 as u64, 2, 2),
            0,
        );
        bytes
    }

    #[test]
    #[ignore = "requires a Metal-capable macOS host"]
    fn shared_texture_removal_is_idempotent_and_removed_key_rebuilds() {
        objc::rc::autoreleasepool(|| {
            let atlas = MetalAtlas::new(Device::system_default().expect("Metal device required"));
            let a = key(1);
            let b = key(2);
            let old_a = insert(&atlas, &a, 0x11);
            let tile_b = insert(&atlas, &b, 0x22);
            assert_eq!(old_a.texture_id, tile_b.texture_id);
            let texture = atlas.metal_texture(old_a.texture_id);

            atlas.remove(&a);
            atlas.remove(&a);
            {
                let state = atlas.0.lock();
                assert!(!state.tiles_by_key.contains_key(&a));
                assert!(state.tiles_by_key.contains_key(&b));
                assert_eq!(state.texture(tile_b.texture_id).live_atlas_keys, 1);
            }
            assert_eq!(pixels(&texture, &tile_b), vec![0x22; 16]);
            let rebuilt = insert(&atlas, &a, 0x33);
            assert_ne!(old_a.tile_id, rebuilt.tile_id);
            assert_eq!(pixels(&texture, &rebuilt), vec![0x33; 16]);
            // Individual regions are deliberately not reused: an already
            // committed draw may still read the old pixels on this texture.
            assert_eq!(pixels(&texture, &old_a), vec![0x11; 16]);

            atlas.remove(&a);
            atlas.remove(&b);
            let state = atlas.0.lock();
            assert!(state.tiles_by_key.is_empty());
            assert!(state.polychrome_textures.textures.iter().all(Option::is_none));
            assert_eq!(state.polychrome_textures.free_list.len(), 1);
        });
    }

    #[test]
    #[ignore = "requires a Metal-capable macOS host"]
    fn last_removal_clears_all_keys_before_reusing_the_texture_slot() {
        objc::rc::autoreleasepool(|| {
            let atlas = MetalAtlas::new(Device::system_default().expect("Metal device required"));
            for index in 0..96usize {
                let a = key(index * 2);
                let b = key(index * 2 + 1);
                let tile_a = insert(&atlas, &a, 0x44);
                let tile_b = insert(&atlas, &b, 0x55);
                assert_eq!(tile_a.texture_id, tile_b.texture_id);
                atlas.remove(&a);
                atlas.remove(&b);
                atlas.remove(&a);
                {
                    let state = atlas.0.lock();
                    assert!(state.tiles_by_key.is_empty());
                    assert_eq!(state.polychrome_textures.textures.len(), 1);
                    assert_eq!(state.polychrome_textures.free_list.len(), 1);
                }
                let rebuilt = insert(&atlas, &a, 0x66);
                assert_eq!(
                    pixels(&atlas.metal_texture(rebuilt.texture_id), &rebuilt),
                    vec![0x66; 16]
                );
                atlas.remove(&a);
            }
        });
    }

    #[test]
    #[ignore = "requires a Metal-capable macOS host"]
    fn committed_draw_keeps_old_texture_after_atlas_slot_is_reused() {
        use block::ConcreteBlock;
        use std::{sync::mpsc, time::Duration};

        struct ReleaseGate {
            event: metal::SharedEvent,
            deadline: mpsc::Sender<()>,
        }
        impl Drop for ReleaseGate {
            fn drop(&mut self) {
                self.event.set_signaled_value(1);
                let _ = self.deadline.send(());
            }
        }

        objc::rc::autoreleasepool(|| {
            let device = Device::system_default().expect("Metal device required");
            let atlas = MetalAtlas::new(device.clone());
            let a = key(1);
            let b = key(2);
            let tile = insert(&atlas, &a, 0x19);
            insert(&atlas, &b, 0x29);
            let texture = atlas.metal_texture(tile.texture_id);
            let queue = device.new_command_queue();
            let output = device.new_buffer(512, metal::MTLResourceOptions::StorageModeShared);
            let event = device.new_shared_event();
            let command = queue.new_command_buffer().to_owned();
            command.encode_wait_for_event(&event, 1);
            let blit = command.new_blit_command_encoder();
            blit.copy_from_texture_to_buffer(
                &texture,
                0,
                0,
                metal::MTLOrigin {
                    x: tile.bounds.origin.x.0 as u64,
                    y: tile.bounds.origin.y.0 as u64,
                    z: 0,
                },
                metal::MTLSize::new(2, 2, 1),
                &output,
                0,
                256,
                512,
                metal::MTLBlitOption::None,
            );
            blit.end_encoding();
            let (completed_tx, completed_rx) = mpsc::channel();
            let completion = ConcreteBlock::new(move |_| {
                let _ = completed_tx.send(());
            })
            .copy();
            command.add_completed_handler(&completion);

            // As in the renderer teardown test, never leave an event wait on
            // the GPU if cleanup or an assertion stalls the test thread.
            let deadline_event = event.to_owned();
            let (deadline_tx, deadline_rx) = mpsc::channel();
            let deadline = std::thread::spawn(move || {
                let timed_out = matches!(
                    deadline_rx.recv_timeout(Duration::from_millis(500)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                );
                deadline_event.set_signaled_value(1);
                timed_out
            });
            let gate = ReleaseGate {
                event,
                deadline: deadline_tx,
            };
            command.commit();
            atlas.remove(&a);
            atlas.remove(&b);
            drop(texture);
            let replacement = insert(&atlas, &key(3), 0x39);
            assert_eq!(replacement.texture_id, tile.texture_id);
            assert_eq!(gate.event.signaled_value(), 0);
            assert_eq!(completed_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
            drop(gate);
            assert!(
                !deadline.join().unwrap(),
                "Atlas cleanup exceeded the GPU gate deadline"
            );
            completed_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("Metal copy did not complete");
            assert_eq!(command.status(), metal::MTLCommandBufferStatus::Completed);
            let bytes = unsafe { std::slice::from_raw_parts(output.contents().cast::<u8>(), 512) };
            assert_eq!(&bytes[..8], &[0x19; 8]);
            assert_eq!(&bytes[256..264], &[0x19; 8]);
        });
    }
}
