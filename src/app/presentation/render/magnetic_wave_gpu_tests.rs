//! Original Wave0/3 spans/packed pixels through the retained Air Display body,
//! shared SurfaceLines snapshots and real GPU output. Component inputs are
//! supplied native state; producer/lift admission is tested by its sim owner.

use crate::app::presentation::render::draw_plan_lowering::{
    NativeDisplayOrder, ObjectBodyDraw, PlannedObjectInstance, lower_object_instances,
};
use crate::app::presentation::render::merge_passes::draw_native_object_pass;
use crate::render::batch::BatchRenderer;
use crate::render::native_surface_format::{DirectDrawPixelFormat, RGB555, RGB565};
use crate::render::shroud_buffer::ShroudBuffer;
use crate::render::surface_line::{SurfaceDraw, SurfaceLineViewport};
use crate::render::tactical_draw_plan::SpriteEncoding;
use crate::render::terrain_draw::TerrainDrawRenderer;
use crate::render::terrain_draw_gpu_tests::{Gpu, camera, encoded, seed_depth_grid};
use crate::render::wave_geometry::{
    WaveClipPath, WaveDraw, WavePixel, WavePolygon, WaveRasterError, polygon_spans, wave_tables,
};
use crate::sim::world::display_layers::{DisplayLayer, DisplayLayers};
use crate::util::sha256::sha256_hex;
use serde_json::Value;

fn corpus() -> &'static Value {
    crate::test_fixture::json("tools/procedural_drawing_oracle/magnetic_wave.json")
}

fn sonic_corpus() -> &'static Value {
    crate::test_fixture::json("tools/procedural_drawing_oracle/sonic_wave.json")
}

fn integer(value: &Value) -> i32 {
    value.as_i64().unwrap().try_into().unwrap()
}

fn array<const N: usize>(value: &Value) -> [i32; N] {
    std::array::from_fn(|i| integer(&value[i]))
}

fn words(value: &Value) -> Vec<u16> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| integer(v).try_into().unwrap())
        .collect()
}

fn hex(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn word_hash(words: &[u16]) -> String {
    sha256_hex(
        &words
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}

fn draw(value: &Value) -> WaveDraw {
    WaveDraw {
        polygon: if value["wave_type"] == 0 {
            WavePolygon::Sonic(std::array::from_fn(|i| array(&value["polygon"][i])))
        } else {
            WavePolygon::Magnetic(std::array::from_fn(|i| array(&value["polygon"][i])))
        },
        source_point: array(&value["source_point"]),
        source_z: integer(&value["source_z"]),
        phase: integer(&value["phase"]),
        direction: integer(&value["direction"]).try_into().unwrap(),
        clip_path: match value["clip_path"].as_str().unwrap() {
            "fast" => WaveClipPath::Fast,
            "fallback" => WaveClipPath::Fallback,
            other => panic!("unknown native clip branch {other}"),
        },
    }
}

fn format(input: &Value) -> DirectDrawPixelFormat {
    match input["format"].as_str().unwrap() {
        "rgb565" => RGB565,
        "rgb555" => RGB555,
        other => panic!("unknown native surface format {other}"),
    }
}

fn expected_words(row: &Value) -> Vec<u16> {
    let input = &row["input"];
    let size = array::<2>(&input["size"]);
    let mut expected = words(&input["background_words"]);
    assert_eq!(expected.len(), (size[0] * size[1]) as usize);
    for changed in row["after"]["pixels"].as_array().unwrap() {
        let [x, y, word] = array::<3>(changed);
        expected[(y * size[0] + x) as usize] = word.try_into().unwrap();
    }
    assert_eq!(
        word_hash(&expected),
        row["after"]["pixel_sha256"].as_str().unwrap(),
        "{} lossless native whole-plane expansion",
        input["name"]
    );
    expected
}

fn rows() -> impl Iterator<Item = &'static Value> {
    corpus()["raster_rows"].as_array().unwrap().iter().chain(
        sonic_corpus()["raster_rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["source_domain"] != "out_of_plane"),
    )
}

#[test]
fn original_wave_tables_and_integer_polygon_spans_are_consumed() {
    let tables = wave_tables();
    for native in [&corpus()["tables"], &sonic_corpus()["tables"]] {
        assert_eq!(
            tables
                .distance
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
            hex(&native["distance"]["hex"]),
            "whole original75F020 distance table"
        );
        assert_eq!(
            tables
                .magnetic
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
            hex(&native["magnetic"]["hex"]),
            "whole original75F020 magnetic table"
        );
        assert_eq!(
            (0..1002)
                .flat_map(|i| tables.magnetic_sine(i).unwrap().to_le_bytes())
                .collect::<Vec<_>>(),
            hex(&native["magnetic_window"]["hex"]),
            "original magnetic +75E8F0 zero coordinate +Sonic initialized read window"
        );
        assert_eq!(
            tables.brightness.to_vec(),
            native["brightness"]["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(integer)
                .collect::<Vec<_>>()
        );
    }
    let mut directions = [false; 8];
    let mut kinds = [false; 2];
    let mut paths = [false; 2];
    for row in rows() {
        for value in row["draws"].as_array().unwrap() {
            let wave = draw(value);
            directions[wave.direction as usize] = true;
            kinds[usize::from(matches!(wave.polygon, WavePolygon::Sonic(_)))] = true;
            paths[usize::from(wave.clip_path == WaveClipPath::Fallback)] = true;
            let spans = polygon_spans(wave.polygon.points());
            let expected = &value["spans"];
            let expected_rows = expected["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(array::<2>)
                .collect::<Vec<_>>();
            assert_eq!(
                spans.rows, expected_rows,
                "{} original63F080/63F2E0",
                row["input"]["name"]
            );
            if !spans.rows.is_empty() {
                assert_eq!(spans.top, integer(&expected["top"]));
            }
        }
    }
    assert!(
        directions.into_iter().all(|covered| covered),
        "all8 native constructor directions"
    );
    assert!(
        paths.into_iter().all(|covered| covered),
        "native fast3 and fallback2 clips"
    );
    assert!(
        kinds.into_iter().all(|covered| covered),
        "both native Wave bodies"
    );
}

/// The Wave0 birth and terminal-decay projections degenerate to the out-and-
/// back slit `[c, source, d, d, source, c]` once fade_in reaches fade_out, and
/// point-blank fire collapses it further. When the source vertex is the
/// vertical extreme (fire within about 17 degrees of screen vertical, the
/// reported dolphin pile-on), a side walks past the maximum vertex and
/// re-ascends the mirrored edge:63F080 keeps executing and its extra63F2E0
/// writes land in the count*8+0x640 allocation slack at63F1DF that the
///75FA90 raster never reads. The consumed first-count rows below are that
/// native output; both shapes panicked here before emission was clamped.
#[test]
fn degenerate_sonic_slit_sides_match_native_consumed_rows() {
    // Source on top, both edge ends below (polygon order c, source, d, d,
    // source, c). Left side: source→target_a fills all 29 rows, then the walk
    // continues around the slit and re-ascends source→target_b; those 30
    // writes land past row 29 and the clamp drops them. Right side:
    // source→target_b minus one fills its 29 rows and stops at the maximum.
    let spans = polygon_spans(&[[-30, 30], [0, 0], [30, 30], [30, 30], [0, 0], [-30, 30]]);
    assert_eq!(spans.top, 1);
    assert_eq!(spans.rows.len(), 29);
    for (row, index) in spans.rows.iter().zip(0..) {
        assert_eq!(row[0], -1 - index, "left chain covers its ascent once");
        assert_eq!(row[1], index, "right chain covers its ascent once");
    }

    // Source at the bottom with both edge ends above: target_a/target_b form
    // the top chain. The left side walks c→source (dx+30 over dy60, so its
    // error walk emits each x twice ascending, -29..0) for all 59 rows, then
    // re-ascends target_b→source past the written rows; the clamp drops those
    // 60 overshoot writes. The right side is the same edge minus one.
    let spans = polygon_spans(&[
        [-30, -30],
        [0, 30],
        [30, -30],
        [30, -30],
        [0, 30],
        [-30, -30],
    ]);
    assert_eq!(spans.top, -29);
    assert_eq!(spans.rows.len(), 59);
    for (row, index) in spans.rows.iter().zip(0..) {
        let left = -29 + index / 2;
        assert_eq!(
            row[0], left,
            "left chain rows follow the diagonal source edge"
        );
        assert_eq!(
            row[1],
            left - 1,
            "the right-side minus-one follows at depth"
        );
    }
}

#[test]
fn original_pixel_linear_aliases_and_initialized_adjacent_window_are_consumed() {
    for row in corpus()["lookup_rows"].as_array().unwrap() {
        let input = &row["input"];
        let point = array::<2>(&input["destination"]);
        // Pixel760190 takes relative lookup coordinates independently of its
        // supplied destination pointer. Express those declared relative inputs
        // at that destination for the shared raster leaf; no Z admission here.
        let wave = WaveDraw {
            polygon: WavePolygon::Magnetic([[0; 2]; 4]),
            source_point: [
                point[0] - integer(&input["delta_x"]),
                point[1] - integer(&input["abs_y"]),
            ],
            source_z: 0,
            phase: integer(&input["phase"]),
            direction: integer(&input["direction"]).try_into().unwrap(),
            clip_path: WaveClipPath::Fast,
        };
        let pixel = wave
            .pixel_operation(
                point,
                array(&input["size"]),
                integer(&input["pitch_words"]),
                0,
            )
            .unwrap();
        let native = &row["operation"];
        assert_eq!(
            pixel.distance_index,
            integer(&native["distance_index"]),
            "{input}"
        );
        assert_eq!(pixel.sine_index, integer(&native["sine_index"]), "{input}");
        assert_eq!(pixel.brightness, integer(&native["brightness"]), "{input}");
        assert_eq!(pixel.source, array::<2>(&native["source"]), "{input}");
    }
}

#[test]
fn original_wave_traversal_displaced_reads_and_depth_admission_match() {
    let mut formats = [false; 2];
    for row in rows() {
        let input = &row["input"];
        let name = &input["name"];
        let size = array::<2>(&input["size"]);
        let pixel_format = format(input);
        formats[usize::from(pixel_format == RGB555)] = true;
        let z = words(&input["z_words"]);
        let alpha = words(&input["alpha_words"]);
        let mut output = words(&input["background_words"]);
        assert_eq!(z.len(), output.len());
        assert_eq!(alpha.len(), output.len());
        let operations = row["operations"].as_array().unwrap();
        let mut admitted = Vec::new();
        for value in row["draws"].as_array().unwrap() {
            draw(value)
                .rasterize(
                    array(&input["clip"]),
                    size,
                    integer(&input["pitch_words"]),
                    integer(&input["z_origin_y"]),
                    |pixel| {
                        let index = (pixel.point[1] * size[0] + pixel.point[0]) as usize;
                        if pixel.candidate_z < z[index] {
                            let source =
                                output[(pixel.source[1] * size[0] + pixel.source[0]) as usize];
                            // Replay the native store only to expose the next Wave's
                            // actual source word. The production WGSL channel gain
                            // is compared with native full planes by the GPU test.
                            let native = &operations[admitted.len()];
                            let after = integer(&native["destination_word"]) as u16;
                            output[index] = after;
                            admitted.push((pixel, source, after));
                        }
                    },
                )
                .unwrap_or_else(|error| {
                    panic!("{name} actual native lookup/source domain: {error:?}")
                });
        }
        assert_eq!(
            output,
            expected_words(row),
            "{name} original Sonic/magnetic full plane"
        );
        assert_eq!(
            admitted.len(),
            operations.len(),
            "{name} native admitted count"
        );
        for ((pixel, source, after), native) in admitted.iter().zip(operations) {
            let expected = WavePixel {
                point: array(&native["point"]),
                source: array(&native["source"]),
                candidate_z: integer(&native["candidate_z"]).try_into().unwrap(),
                brightness: integer(&native["brightness"]),
                distance_index: integer(&native["distance_index"]),
                sine_index: integer(&native["sine_index"]),
            };
            assert_eq!(
                *pixel, expected,
                "{name} native traversal/lookup/read address"
            );
            assert_eq!(*source, integer(&native["source_word"]) as u16);
            assert_eq!(*after, integer(&native["destination_word"]) as u16);
        }
        for key in ["z", "alpha"] {
            assert_eq!(
                row["after"][format!("{key}_before_sha256")],
                row["after"][format!("{key}_after_sha256")],
                "{name} native unchanged {key}"
            );
        }
        // These full native states establish non-spending by the executed
        //component, not a claim that Rust's whole-app streams were compared.
        for stream in ["main", "scenario", "mapgen"] {
            let before = row["rng_before"][stream].as_str().unwrap();
            assert_eq!(before.len(), 0x3f4 * 2);
            assert_eq!(
                before,
                row["rng_after"][stream].as_str().unwrap(),
                "{name} {stream}"
            );
        }
    }
    assert!(
        formats.into_iter().all(|covered| covered),
        "both native packed formats"
    );
}

#[test]
fn original_sonic_out_of_plane_sample_is_an_explicit_domain_error() {
    let mut rejected = 0;
    for row in sonic_corpus()["raster_rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["source_domain"] == "out_of_plane")
    {
        let input = &row["input"];
        let size = array::<2>(&input["size"]);
        let pitch = integer(&input["pitch_words"]);
        let native = row["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|op| {
                let [x, y] = array::<2>(&op["source"]);
                x < 0 || x >= size[0] || y < 0 || y >= size[1]
            })
            .unwrap();
        let [x, y] = array::<2>(&native["source"]);
        let error = draw(&row["draws"][0])
            .rasterize(
                array(&input["clip"]),
                size,
                pitch,
                integer(&input["z_origin_y"]),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error, WaveRasterError::SourceWord(y * pitch + x));
        rejected += 1;
    }
    assert_eq!(rejected, 1, "the declared original small-surface refusal");
}

#[test]
#[ignore = "requires GPU; original Wave0/3 words, Air Display order, A/Z and bounded source snapshots"]
fn wave_pixels_reach_retained_display_and_shared_gpu_with_immutable_z_and_a() {
    let gpu = Gpu::new();
    for row in rows().filter(|row| row["input"]["format"] == "rgb565") {
        let input = &row["input"];
        let name = &input["name"];
        let size = array::<2>(&input["size"]).map(|v| v.try_into().unwrap());
        let background = words(&input["background_words"]);
        let z = words(&input["z_words"]);
        let alpha = words(&input["alpha_words"]);
        let expected = expected_words(row);
        let waves = row["draws"]
            .as_array()
            .unwrap()
            .iter()
            .map(draw)
            .collect::<Vec<_>>();
        for format in [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let batch = BatchRenderer::new_with_device(&gpu.device, &gpu.queue, format);
            batch.write_camera(&gpu.queue, camera(size));
            let color = gpu.target(size, format);
            let cv = color.create_view(&Default::default());
            let depth = gpu.target(size, wgpu::TextureFormat::Depth32Float);
            let dv = depth.create_view(&Default::default());
            let mut terrain = TerrainDrawRenderer::new(&gpu.device, &gpu.queue, format, &batch);
            terrain.prepare(&gpu.device, &color, &dv, batch.camera_uniform());
            let a = ShroudBuffer::fixture(
                &gpu.device,
                &gpu.queue,
                size,
                [0.; 2],
                &alpha
                    .iter()
                    .map(|v| (*v).try_into().unwrap())
                    .collect::<Vec<_>>(),
            );
            let a_before = gpu.read_float_texels(a.gpu_source().0, [0, 0], size);
            for limit in [None, Some(7)] {
                terrain.set_surface_line_operation_limit(limit);
                terrain.begin_surface_lines();
                let mut display = DisplayLayers::default();
                // Opposite storage/ID order cannot replace native Air append order.
                let ids = (1..=waves.len() as u64).rev().collect::<Vec<_>>();
                for &id in &ids {
                    assert!(display.submit(id, Some(DisplayLayer::AIR), &|_| 0));
                }
                let order = NativeDisplayOrder::from_display(&display);
                let mut parents = waves
                    .iter()
                    .zip(&ids)
                    .map(|(&wave, &id)| {
                        let mut parent = PlannedObjectInstance::object(
                            order.object_draw(id, SpriteEncoding::Plain).unwrap(),
                            Vec::new(),
                        );
                        parent.body_draw = Some(ObjectBodyDraw::Surface(SurfaceDraw::Wave(wave)));
                        parent
                    })
                    .collect::<Vec<_>>();
                parents.reverse();
                let mut layers = lower_object_instances(parents);
                let air = &mut layers[3];
                for draws in &air.surface_draws {
                    air.line_batches.push(terrain.prepare_surface_lines(
                        &gpu.device,
                        &gpu.queue,
                        std::iter::empty(),
                        draws.iter().copied(),
                        &[],
                        &[],
                        &[],
                        SurfaceLineViewport {
                            camera: [0; 2],
                            clip: array(&input["clip"]),
                            z_origin_y: integer(&input["z_origin_y"]),
                            zoom: 1.,
                        },
                        |_| false,
                        |_| panic!("Wave must never query A"),
                    ));
                }
                let initial = background
                    .iter()
                    .flat_map(|&word| encoded(word, format))
                    .collect::<Vec<_>>();
                gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &color,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &initial,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(size[0] * 4),
                        rows_per_image: Some(size[1]),
                    },
                    color.size(),
                );
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                seed_depth_grid(&gpu, &mut encoder, &dv, &z, size[0]);
                draw_native_object_pass(
                    &mut encoder,
                    crate::app::render::merge_passes::ObjectDrawPhase::Display,
                    &cv,
                    &dv,
                    &mut terrain,
                    [0, 0, size[0], size[1]],
                    &batch,
                    None,
                    air,
                    None,
                    None,
                    None,
                    None,
                    None,
                    batch.default_zshape_bind_group(),
                    None,
                    None,
                );
                let reads = [
                    gpu.read(&mut encoder, &color),
                    gpu.read(&mut encoder, &depth),
                ];
                let output = gpu.finish(encoder, &reads, size);
                let expected_color = expected
                    .iter()
                    .flat_map(|&word| encoded(word, format))
                    .collect::<Vec<_>>();
                assert_eq!(
                    output[0], expected_color,
                    "{name} {format:?} limit{limit:?}: native whole color"
                );
                let expected_depth = z
                    .iter()
                    .flat_map(|&word| crate::render::native_z::stored_depth(word).to_le_bytes())
                    .collect::<Vec<_>>();
                assert_eq!(
                    output[1], expected_depth,
                    "{name} {format:?}: whole Z unchanged"
                );
                assert_eq!(
                    gpu.read_float_texels(a.gpu_source().0, [0, 0], size),
                    a_before,
                    "{name} {format:?}: whole A source unchanged"
                );
            }
        }
    }
}

/// Dolphin pile-on scaling diagnostic for the reported freeze (#3103): every
/// live Sonic wave forces its own surface-line batch (one snapshot and one
/// ordered blend pass, one chunk sort/upload) because its displaced reads must
/// observe every preceding completed draw, and dolphins re-fire as each
/// ~101-tick wave expires. The frame cost must grow linearly with the live
/// wave count: upload() only sorts and re-writes the chunk staged since the
/// last upload, so quadratic growth would mean a batching regression. Each
/// doubling of N multiplies a linear cost by ~2 and a quadratic cost by ~4,
/// so 2.8 separates them with noise headroom; the absolute curve is printed
/// for the report.
#[test]
#[ignore = "slow; GPU pile-on scaling diagnostic, uncontrolled shared-host timing"]
fn wave_pile_on_frame_cost_grows_linearly_with_concurrent_waves() {
    use std::time::Duration;

    let gpu = Gpu::new();
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let size = [800u32, 600];
    let batch = BatchRenderer::new_with_device(&gpu.device, &gpu.queue, format);
    batch.write_camera(&gpu.queue, camera(size));
    let color = gpu.target(size, format);
    let cv = color.create_view(&Default::default());
    let depth = gpu.target(size, wgpu::TextureFormat::Depth32Float);
    let dv = depth.create_view(&Default::default());
    let mut terrain = TerrainDrawRenderer::new(&gpu.device, &gpu.queue, format, &batch);
    terrain.prepare(&gpu.device, &color, &dv, batch.camera_uniform());
    // EMPTY_Z stores as the native maximum, so every wave pixel passes the
    // strict candidate<stored admission and the raster does full work.
    {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        seed_depth_grid(
            &gpu,
            &mut encoder,
            &dv,
            &vec![crate::render::native_z::EMPTY_Z; (size[0] * size[1]) as usize],
            size[0],
        );
        gpu.queue.submit([encoder.finish()]);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }
    // The corpus sonic_clipped polygon: a mature ~196x106 native wave fan with
    // every displaced source read inside the surface and every table lookup in
    // domain, so no draw rolls back.
    let wave = SurfaceDraw::Wave(WaveDraw {
        polygon: WavePolygon::Sonic([
            [138, 74],
            [130, 82],
            [114, 86],
            [-58, 6],
            [-50, -1],
            [-34, -6],
        ]),
        source_point: [40, 40],
        source_z: 0,
        phase: 100,
        direction: 3,
        clip_path: WaveClipPath::Fast,
    });
    let viewport = SurfaceLineViewport {
        camera: [0; 2],
        clip: [0, 0, size[0] as i32, size[1] as i32],
        z_origin_y: 0,
        zoom: 1.,
    };
    let mut medians = Vec::new();
    for count in [1usize, 2, 4, 8, 16, 32] {
        let mut samples = Vec::new();
        for frame in 0..12 {
            let begin = std::time::Instant::now();
            terrain.begin_surface_lines();
            let prepared = terrain.prepare_surface_lines(
                &gpu.device,
                &gpu.queue,
                std::iter::empty(),
                std::iter::repeat(wave).take(count),
                &[],
                &[],
                &[],
                viewport,
                |_| false,
                // Waves read the destination but never query A.
                |_| panic!("Wave must never query A"),
            );
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            terrain.draw_surface_lines(&mut encoder, &cv, prepared);
            let submission = gpu.queue.submit([encoder.finish()]);
            gpu.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(Duration::from_secs(60)),
                })
                .unwrap();
            samples.push(begin.elapsed().as_secs_f64() * 1000.);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let kept = &samples[2..];
        medians.push((count, kept[kept.len() / 2]));
    }
    for (count, ms) in &medians {
        eprintln!("wave pile-on frame: N={count} median {ms:.2} ms");
    }
    for pair in medians.windows(2) {
        let ratio = pair[1].1 / pair[0].1;
        assert!(
            ratio < 2.8,
            "per-frame cost grew {ratio:.2}x from N={} to N={} waves: batching regression",
            pair[0].0,
            pair[1].0,
        );
    }
}
