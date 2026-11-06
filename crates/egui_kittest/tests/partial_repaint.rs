//! Repainting only what [`egui::epaint::DamageTracker`] finds changed, with
//! [`egui_wgpu::Renderer::render_damaged`] over the last frame's pixels, gives the same pixels as
//! repainting the whole frame. This is what `egui_wgpu::PartialRepaint` does in the native
//! painter, without the window.
#![cfg(feature = "wgpu")]

use egui::epaint::{ClippedShape, Damage, DamageTracker, Shape};
use egui::{Color32, Pos2, Rect, Vec2, emath::TSTransform, vec2};
use egui_wgpu::{RendererOptions, ScreenDescriptor, wgpu};

const SIZE: [u32; 2] = [320, 240];
const CLEAR: Color32 = Color32::from_rgb(30, 32, 36);

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

fn target(device: &wgpu::Device, format: wgpu::TextureFormat) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("partial repaint test"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Target { texture, view }
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = SIZE[0] * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded * SIZE[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(SIZE[1]),
            },
        },
        wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |r| {
        r.expect("the readback buffer maps");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(core::time::Duration::from_secs(30)),
        })
        .expect("the GPU finishes");
    let data = buffer
        .slice(..)
        .get_mapped_range()
        .expect("the buffer is mapped");
    data.chunks(padded as usize)
        .flat_map(|r| r[..row as usize].to_vec())
        .collect()
}

/// One frame of a small window: hovering moves along a row of buttons, a label changes, a
/// window opens and closes, and some frames repeat the one before.
fn frame(ctx: &egui::Context, step: usize, zoom: f32) -> egui::FullOutput {
    // Every state is shown twice in a row: the second frame repeats the first.
    let step = step / 2;
    let pointer = [
        Pos2::new(5.0, 5.0),
        Pos2::new(30.0, 20.0),
        Pos2::new(30.0, 20.0),
        Pos2::new(110.0, 20.0),
        Pos2::new(190.0, 20.0),
        Pos2::new(300.0, 200.0),
    ][step % 6];
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            Pos2::ZERO,
            vec2(SIZE[0] as f32, SIZE[1] as f32) / zoom,
        )),
        events: vec![egui::Event::PointerMoved(pointer)],
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| {
        ui.horizontal(|ui| {
            for name in ["One", "Two", "Three"] {
                let _ = ui.button(name);
            }
        });
        ui.label(format!("Frame group {}", step / 4));
        ui.add(egui::Slider::new(&mut ((step % 7) as f32), 0.0..=10.0));
        if (step / 5) % 2 == 1 {
            egui::Window::new("Popup")
                .fixed_pos(Pos2::new(140.0, 90.0))
                .show(ui.ctx(), |ui| ui.label("Shown"));
        }
    })
}

#[test]
fn repainting_only_the_damage_matches_repainting_everything() {
    // Fractional scales put shape edges between pixels, where anti-aliasing reaches past them.
    for zoom in [1.0, 1.25, 1.5] {
        repaint_frames(zoom);
    }
}

fn repaint_frames(zoom: f32) {
    let render_state = egui_kittest::wgpu::create_render_state(
        egui_kittest::wgpu::default_wgpu_setup(),
        RendererOptions {
            // Noise would differ between two paints of the same pixel; partial repaint
            // targets software rasterizers, which go without it.
            dithering: false,
            ..Default::default()
        },
    );
    let device = &render_state.device;
    let queue = &render_state.queue;
    let format = render_state.target_format;
    let full = target(device, format);
    let partial = target(device, format);

    let ctx = egui::Context::default();
    ctx.all_styles_mut(|s| s.animation_time = 0.0);
    ctx.set_zoom_factor(zoom);
    let mut tracker = DamageTracker::default();
    let (mut skipped, mut partials) = (0, 0);

    for step in 0..80 {
        let output = frame(&ctx, step, zoom);
        let ppp = output.pixels_per_point;
        let screen = ScreenDescriptor {
            size_in_pixels: SIZE,
            pixels_per_point: ppp,
        };
        let background =
            Rect::from_min_size(Pos2::ZERO, Vec2::new(SIZE[0] as f32, SIZE[1] as f32) / ppp)
                .expand(1.0);
        let mut shapes = output.shapes;
        shapes.insert(
            0,
            ClippedShape {
                clip_rect: background,
                shape: Shape::rect_filled(background, 0.0, CLEAR),
                transform_after_tessellation: TSTransform::IDENTITY,
            },
        );
        let mut textures_delta = output.textures_delta;
        let damage = tracker.update(&shapes, ppp, &textures_delta);

        let mut renderer = render_state.renderer.write();
        #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
        for (id, deltas) in textures_delta.set.drain() {
            for delta in deltas {
                renderer.update_texture(device, queue, id, &delta);
            }
        }
        let primitives = ctx.tessellate(shapes, ppp);

        let mut paint = |target: &Target, damage: Option<&Damage>| {
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            let user = renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen);
            {
                let load = if matches!(damage, Some(Damage::Partial(_))) {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                };
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target.view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                match damage {
                    Some(Damage::Partial(region)) => {
                        renderer.render_damaged(&mut pass, &primitives, &screen, region.rects());
                    }
                    _ => renderer.render(&mut pass, &primitives, &screen),
                }
            }
            queue.submit(core::iter::chain(user, [encoder.finish()]));
        };

        paint(&full, None);
        match &damage {
            Damage::None => skipped += 1,
            Damage::Partial(_) => {
                partials += 1;
                paint(&partial, Some(&damage));
            }
            Damage::Full => paint(&partial, Some(&damage)),
        }
        #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
        for id in textures_delta.free.drain() {
            renderer.free_texture(&id);
        }
        drop(renderer);

        let (a, b) = (
            read(device, queue, &full.texture),
            read(device, queue, &partial.texture),
        );
        let differing = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x != y).count();
        assert_eq!(
            differing, 0,
            "zoom {zoom}, frame {step} ({damage:?}): {differing} pixels differ"
        );
    }
    assert!(skipped > 0, "some frames repeat the one before");
    assert!(partials > 0, "and some change only a part");
}
