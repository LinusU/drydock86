//! Headless rendering checks (require a wgpu adapter; skipped with a message if none).
use drydock_gfx::*;

fn gpu() -> Option<std::sync::Arc<Gpu>> {
    match Gpu::new(&GpuOptions::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn tl(x: f32, y: f32, z: f32, c: [u8; 4]) -> Vertex {
    Vertex {
        pos: [x, y, z, 1.0],
        diffuse: c,
        ..Default::default()
    }
}

fn base_uniforms(w: f32, h: f32) -> Uniforms {
    let mut u = Uniforms::default();
    u.rt = [w, h, w, h];
    u.flags[0] = F_PRETRANSFORMED | F_HAS_DIFFUSE;
    u.stages[0].ops = [3, 2, 0, 3]; // SELECTARG2(diffuse) for color and alpha
    u.stages[0].ops2 = [2, 0, 0, 0];
    u
}

#[test]
fn pretransformed_quad_covers_exact_pixels() {
    let Some(g) = gpu() else { return };
    let mut r = Renderer::new(g);
    r.ensure_texture(1, 8, 8, 1, TexFormat::Color);
    r.upload(1, 0, 8, 8, &[0u8; 8 * 8 * 4]);
    let u = base_uniforms(8.0, 8.0);
    // Quad covering pixels x in [2,6), y in [1,3) using D3D integer pixel centres:
    // edges at 2-0.5? No: with centres at integers, pixel 2 is covered by an edge at 1.5.
    let red = [255, 0, 0, 255];
    let v = [
        tl(1.5, 0.5, 0.0, red),
        tl(5.5, 0.5, 0.0, red),
        tl(1.5, 2.5, 0.0, red),
        tl(5.5, 2.5, 0.0, red),
    ];
    let key = PipeKey {
        topology: Topology::Triangles,
        blend: None,
        depth: None,
        cull: 1,
        color_write: true,
    };
    let vp = Viewport {
        x: 0.0,
        y: 0.0,
        w: 8.0,
        h: 8.0,
        min_z: 0.0,
        max_z: 1.0,
    };
    r.draw(
        1,
        None,
        key,
        &u,
        &v,
        &[0, 1, 2, 2, 1, 3],
        [None, None],
        [SamplerKey::default(); 2],
        vp,
        [0, 0, 8, 8],
    );
    let (_, _, px) = r.readback(1).unwrap();
    let at = |x: usize, y: usize| px[(y * 8 + x) * 4];
    for y in 0..8 {
        for x in 0..8 {
            let inside = (2..6).contains(&x) && (1..3).contains(&y);
            assert_eq!(at(x, y) == 255, inside, "pixel ({x},{y})");
        }
    }
}

#[test]
fn depth_test_rejects_farther_geometry() {
    let Some(g) = gpu() else { return };
    let mut r = Renderer::new(g);
    r.ensure_texture(1, 4, 4, 1, TexFormat::Color);
    r.upload(1, 0, 4, 4, &[0u8; 64]);
    r.ensure_texture(2, 4, 4, 1, TexFormat::Depth);
    r.clear(1, Some(2), [0, 0, 4, 4], None, Some(1.0));
    let u = base_uniforms(4.0, 4.0);
    let key = PipeKey {
        topology: Topology::Triangles,
        blend: None,
        depth: Some(DepthState {
            func: 4,
            write: true,
            bias: 0,
        }),
        cull: 1,
        color_write: true,
    };
    let vp = Viewport {
        x: 0.0,
        y: 0.0,
        w: 4.0,
        h: 4.0,
        min_z: 0.0,
        max_z: 1.0,
    };
    let quad = |z: f32, c: [u8; 4]| {
        [
            tl(-0.5, -0.5, z, c),
            tl(3.5, -0.5, z, c),
            tl(-0.5, 3.5, z, c),
            tl(3.5, 3.5, z, c),
        ]
    };
    let idx = [0, 1, 2, 2, 1, 3];
    r.draw(
        1,
        Some(2),
        key,
        &u,
        &quad(0.25, [0, 255, 0, 255]),
        &idx,
        [None, None],
        [SamplerKey::default(); 2],
        vp,
        [0, 0, 4, 4],
    );
    r.draw(
        1,
        Some(2),
        key,
        &u,
        &quad(0.75, [255, 0, 0, 255]),
        &idx,
        [None, None],
        [SamplerKey::default(); 2],
        vp,
        [0, 0, 4, 4],
    );
    let (_, _, px) = r.readback(1).unwrap();
    assert_eq!(&px[0..4], &[0, 255, 0, 255]);
    let (_, _, d) = r.readback(2).unwrap();
    let z = f32::from_le_bytes(d[0..4].try_into().unwrap());
    assert!((z - 0.25).abs() < 1e-6, "depth {z}");
}

#[test]
fn texture_modulate_and_alpha_test() {
    let Some(g) = gpu() else { return };
    let mut r = Renderer::new(g);
    r.ensure_texture(1, 2, 1, 1, TexFormat::Color);
    r.upload(1, 0, 2, 1, &[0u8; 8]);
    r.ensure_texture(5, 2, 1, 1, TexFormat::Color);
    // Left texel opaque white, right texel alpha 0.
    r.upload(5, 0, 2, 1, &[255, 255, 255, 255, 255, 255, 255, 0]);
    let mut u = base_uniforms(2.0, 1.0);
    u.flags[0] |= F_ALPHATEST;
    u.flags[1] = 5; // GREATER
    u.alpha_fog[0] = 0.0;
    u.stages[0].ops = [4, 2, 0, 4]; // MODULATE(texture, diffuse)
    u.stages[0].ops2 = [2, 0, 0, 1];
    let c = [128, 64, 255, 255];
    let mut v = [
        tl(-0.5, -0.5, 0.0, c),
        tl(1.5, -0.5, 0.0, c),
        tl(-0.5, 0.5, 0.0, c),
        tl(1.5, 0.5, 0.0, c),
    ];
    v[0].uv0 = [0.0, 0.0];
    v[1].uv0 = [1.0, 0.0];
    v[2].uv0 = [0.0, 1.0];
    v[3].uv0 = [1.0, 1.0];
    let key = PipeKey {
        topology: Topology::Triangles,
        blend: None,
        depth: None,
        cull: 1,
        color_write: true,
    };
    let vp = Viewport {
        x: 0.0,
        y: 0.0,
        w: 2.0,
        h: 1.0,
        min_z: 0.0,
        max_z: 1.0,
    };
    let samp = SamplerKey {
        mag: 1,
        min: 1,
        mip: 1,
        addr_u: 3,
        addr_v: 3,
    };
    r.draw(
        1,
        None,
        key,
        &u,
        &v,
        &[0, 1, 2, 2, 1, 3],
        [Some(5), None],
        [samp, samp],
        vp,
        [0, 0, 2, 1],
    );
    let (_, _, px) = r.readback(1).unwrap();
    assert_eq!(&px[0..4], &[128, 64, 255, 255]);
    assert_eq!(&px[4..8], &[0, 0, 0, 0], "alpha-tested texel must be discarded");
}

/// D3D cull modes refer to the winding seen on screen (y down): CULL_CCW (3) keeps
/// triangles whose screen-space vertices run clockwise, CULL_CW (2) the opposite.
#[test]
fn cull_modes_follow_screen_winding() {
    let Some(g) = gpu() else { return };
    let mut r = Renderer::new(g);
    let vp = Viewport {
        x: 0.0,
        y: 0.0,
        w: 4.0,
        h: 4.0,
        min_z: 0.0,
        max_z: 1.0,
    };
    let u = base_uniforms(4.0, 4.0);
    let w = [255, 255, 255, 255];
    // Clockwise on screen: top-left, top-right, bottom-left.
    let cw = [tl(-0.5, -0.5, 0.0, w), tl(3.5, -0.5, 0.0, w), tl(-0.5, 3.5, 0.0, w)];
    for (cull, idx, visible) in [
        (3, [0, 1, 2], true),
        (3, [0, 2, 1], false),
        (2, [0, 1, 2], false),
        (2, [0, 2, 1], true),
    ] {
        r.ensure_texture(1, 4, 4, 1, TexFormat::Color);
        r.upload(1, 0, 4, 4, &[0u8; 64]);
        let key = PipeKey {
            topology: Topology::Triangles,
            blend: None,
            depth: None,
            cull,
            color_write: true,
        };
        r.draw(
            1,
            None,
            key,
            &u,
            &cw,
            &idx,
            [None, None],
            [SamplerKey::default(); 2],
            vp,
            [0, 0, 4, 4],
        );
        let (_, _, px) = r.readback(1).unwrap();
        assert_eq!(px[0] == 255, visible, "cull {cull} order {idx:?}");
    }
}

#[test]
fn depth_upload_round_trips() {
    let Some(g) = gpu() else { return };
    let mut r = Renderer::new(g);
    r.ensure_texture(2, 4, 2, 1, TexFormat::Depth);
    let z: Vec<f32> = (0..8).map(|i| i as f32 / 8.0).collect();
    r.upload_depth(2, 4, 2, &z);
    let (_, _, d) = r.readback(2).unwrap();
    for (i, c) in d.chunks_exact(4).enumerate() {
        let v = f32::from_le_bytes(c.try_into().unwrap());
        assert!((v - z[i]).abs() < 1e-6, "texel {i}: {v} != {}", z[i]);
    }
}
