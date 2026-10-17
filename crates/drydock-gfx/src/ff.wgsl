// Fixed-function pipeline emulation (legacy Direct3D 7 semantics).
//
// Matrices are uploaded in D3D memory layout; with WGSL's column-major interpretation,
// `m * v` computes the D3D row-vector product `v * M`.

struct Light {
    kind: vec4<u32>,          // x: type (0 = off, 1 point, 2 spot, 3 directional)
    diffuse: vec4<f32>,
    specular: vec4<f32>,
    ambient: vec4<f32>,
    position: vec4<f32>,      // view space
    direction: vec4<f32>,     // view space, normalized, pointing from the light
    params: vec4<f32>,        // range, falloff, att0, att1
    params2: vec4<f32>,       // att2, cos(theta/2), cos(phi/2), 0
};

struct Stage {
    ops: vec4<u32>,           // colorop, colorarg1, colorarg2, alphaop
    ops2: vec4<u32>,          // alphaarg1, alphaarg2, texcoord index (+texgen << 16), has texture
    tt: vec4<u32>,            // texture transform count, projected, 0, 0
    texmat: mat4x4<f32>,
};

struct U {
    world: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    rt: vec4<f32>,            // target width, height, viewport w, viewport h
    flags: vec4<u32>,         // x: bits, y: alpha func, z: fog mode, w: fog table source
    alpha_fog: vec4<f32>,     // alpha ref, fog start, fog end, fog density
    fog_color: vec4<f32>,
    ambient: vec4<f32>,
    tfactor: vec4<f32>,
    mat_diffuse: vec4<f32>,
    mat_ambient: vec4<f32>,
    mat_specular: vec4<f32>,
    mat_emissive: vec4<f32>,
    mat_power: vec4<f32>,
    color_src: vec4<u32>,     // diffuse, ambient, specular, emissive source (0 material, 1 color1, 2 color2)
    lights: array<Light, 8>,
    stages: array<Stage, 2>,
};

const F_PRETRANSFORMED: u32 = 1u;
const F_LIGHTING: u32 = 2u;
const F_HAS_NORMAL: u32 = 4u;
const F_HAS_DIFFUSE: u32 = 8u;
const F_HAS_SPECULAR: u32 = 16u;
const F_SPECULAR_ENABLE: u32 = 32u;
const F_LOCAL_VIEWER: u32 = 64u;
const F_NORMALIZE: u32 = 128u;
const F_COLORKEY: u32 = 256u;
const F_ALPHATEST: u32 = 512u;
const F_FOG: u32 = 1024u;
const F_COLOR_VERTEX: u32 = 2048u;
const F_CLEAR: u32 = 4096u;

@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex0: texture_2d<f32>;
@group(1) @binding(1) var samp0: sampler;
@group(1) @binding(2) var tex1: texture_2d<f32>;
@group(1) @binding(3) var samp1: sampler;

struct VIn {
    @location(0) pos: vec4<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) diffuse: vec4<f32>,
    @location(3) specular: vec4<f32>,
    @location(4) uv0: vec2<f32>,
    @location(5) uv1: vec2<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) diffuse: vec4<f32>,
    @location(1) specular: vec4<f32>,
    // Per-stage coordinates (after generation and transform), xy + projective w.
    @location(2) uv0: vec3<f32>,
    @location(3) uv1: vec3<f32>,
    @location(4) fog_depth: vec2<f32>,  // x: vertex fog factor, y: eye depth (or z)
};

fn src_color(sel: u32, mat: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>) -> vec4<f32> {
    if (sel == 1u) { return c1; }
    if (sel == 2u) { return c2; }
    return mat;
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    let bits = u.flags.x;
    var diffuse = select(vec4<f32>(1.0), v.diffuse, (bits & F_HAS_DIFFUSE) != 0u);
    var specular = select(vec4<f32>(0.0), v.specular, (bits & F_HAS_SPECULAR) != 0u);
    if ((bits & F_PRETRANSFORMED) != 0u) {
        // Screen-space vertex: D3D pixel centres are at integer coordinates.
        let rhw = select(1.0, v.pos.w, v.pos.w > 0.0);
        let w = 1.0 / rhw;
        let nx = (v.pos.x + 0.5) / u.rt.x * 2.0 - 1.0;
        let ny = 1.0 - (v.pos.y + 0.5) / u.rt.y * 2.0;
        o.clip = vec4<f32>(nx * w, ny * w, v.pos.z * w, w);
        o.fog_depth = vec2<f32>(specular.a, w);
    } else {
        let wp = u.world * vec4<f32>(v.pos.xyz, 1.0);
        let vp = u.view * wp;
        var clip = u.proj * vp;
        // Pixel-centre convention of the legacy viewport transform.
        clip.x = clip.x + clip.w / u.rt.z;
        clip.y = clip.y - clip.w / u.rt.w;
        o.clip = clip;
        o.fog_depth = vec2<f32>(specular.a, vp.z);
        if ((bits & F_LIGHTING) != 0u) {
            var n = vec3<f32>(0.0, 0.0, 0.0);
            if ((bits & F_HAS_NORMAL) != 0u) {
                n = (u.normal_mat * vec4<f32>(v.normal, 0.0)).xyz;
                if ((bits & F_NORMALIZE) != 0u) {
                    n = normalize(n);
                }
            }
            let cv = (bits & F_COLOR_VERTEX) != 0u;
            let c1 = diffuse;
            let c2 = specular;
            var md = u.mat_diffuse;
            var ma = u.mat_ambient;
            var ms = u.mat_specular;
            var me = u.mat_emissive;
            if (cv) {
                md = src_color(u.color_src.x, u.mat_diffuse, c1, c2);
                ma = src_color(u.color_src.y, u.mat_ambient, c1, c2);
                ms = src_color(u.color_src.z, u.mat_specular, c1, c2);
                me = src_color(u.color_src.w, u.mat_emissive, c1, c2);
            }
            var amb = u.ambient.rgb;
            var dif = vec3<f32>(0.0);
            var spe = vec3<f32>(0.0);
            let p = vp.xyz;
            for (var i = 0u; i < 8u; i = i + 1u) {
                let li = u.lights[i];
                let kind = li.kind.x;
                if (kind == 0u) { continue; }
                var l: vec3<f32>;
                var att = 1.0;
                if (kind == 3u) {
                    l = -li.direction.xyz;
                } else {
                    let d = li.position.xyz - p;
                    let dist = length(d);
                    if (dist > li.params.x) { continue; }
                    l = d / max(dist, 1e-6);
                    att = 1.0 / max(li.params.z + li.params.w * dist + li.params2.x * dist * dist, 1e-6);
                    if (kind == 2u) {
                        let rho = dot(-l, li.direction.xyz);
                        let ct = li.params2.y;
                        let cp = li.params2.z;
                        if (rho <= cp) {
                            att = 0.0;
                        } else if (rho < ct) {
                            att = att * pow((rho - cp) / max(ct - cp, 1e-6), li.params.y);
                        }
                    }
                }
                amb = amb + li.ambient.rgb * att;
                let ndl = max(dot(n, l), 0.0);
                dif = dif + li.diffuse.rgb * ndl * att;
                if ((bits & F_SPECULAR_ENABLE) != 0u && ndl > 0.0) {
                    var vdir = vec3<f32>(0.0, 0.0, -1.0);
                    if ((bits & F_LOCAL_VIEWER) != 0u) {
                        vdir = normalize(-p);
                    }
                    let h = normalize(vdir + l);
                    spe = spe + li.specular.rgb * pow(max(dot(n, h), 0.0), u.mat_power.x) * att;
                }
            }
            let col = me.rgb + ma.rgb * amb + md.rgb * dif;
            diffuse = vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), md.a);
            specular = vec4<f32>(clamp(ms.rgb * spe, vec3<f32>(0.0), vec3<f32>(1.0)), specular.a);
        }
    }
    o.diffuse = diffuse;
    o.specular = specular;
    // Camera-space inputs for texture coordinate generation.
    var cs_pos = vec3<f32>(0.0);
    var cs_n = vec3<f32>(0.0, 0.0, 1.0);
    if ((bits & F_PRETRANSFORMED) == 0u) {
        cs_pos = (u.view * (u.world * vec4<f32>(v.pos.xyz, 1.0))).xyz;
        cs_n = normalize((u.normal_mat * vec4<f32>(v.normal, 0.0)).xyz + vec3<f32>(0.0, 0.0, 1e-9));
    }
    o.uv0 = stage_coords(0u, v, cs_pos, cs_n);
    o.uv1 = stage_coords(1u, v, cs_pos, cs_n);
    return o;
}

fn stage_coords(s: u32, v: VIn, cs_pos: vec3<f32>, cs_n: vec3<f32>) -> vec3<f32> {
    let st = u.stages[s];
    let tci = st.ops2.z;
    let gen = tci >> 16u;
    var t: vec4<f32>;
    if (gen == 1u) {
        t = vec4<f32>(cs_n, 1.0);
    } else if (gen == 2u) {
        t = vec4<f32>(cs_pos, 1.0);
    } else if (gen == 3u) {
        let e = normalize(cs_pos + vec3<f32>(0.0, 0.0, 1e-9));
        t = vec4<f32>(reflect(e, cs_n), 1.0);
    } else {
        let src = select(v.uv0, v.uv1, (tci & 0xffffu) == 1u);
        t = vec4<f32>(src, 1.0, 0.0);
    }
    let count = st.tt.x;
    if (count == 0u) {
        return vec3<f32>(t.xy, 1.0);
    }
    let r = st.texmat * t;
    if (st.tt.y != 0u) {
        // Projected: divide by the last used component.
        var w = r.w;
        if (count == 2u) { w = r.y; }
        if (count == 3u) { w = r.z; }
        return vec3<f32>(r.xy, w);
    }
    return vec3<f32>(r.xy, 1.0);
}

fn arg(a: u32, diffuse: vec4<f32>, current: vec4<f32>, texel: vec4<f32>, spec: vec4<f32>, temp: vec4<f32>) -> vec4<f32> {
    var r: vec4<f32>;
    switch (a & 15u) {
        case 0u: { r = diffuse; }
        case 1u: { r = current; }
        case 2u: { r = texel; }
        case 3u: { r = u.tfactor; }
        case 4u: { r = spec; }
        case 5u: { r = temp; }
        default: { r = current; }
    }
    if ((a & 16u) != 0u) { r = vec4<f32>(1.0) - r; }
    if ((a & 32u) != 0u) { r = vec4<f32>(r.a); }
    return r;
}

fn op(o: u32, a1: vec4<f32>, a2: vec4<f32>, diffuse: vec4<f32>, current: vec4<f32>, texel: vec4<f32>) -> vec4<f32> {
    switch (o) {
        case 2u: { return a1; }
        case 3u: { return a2; }
        case 4u: { return a1 * a2; }
        case 5u: { return a1 * a2 * 2.0; }
        case 6u: { return a1 * a2 * 4.0; }
        case 7u: { return a1 + a2; }
        case 8u: { return a1 + a2 - vec4<f32>(0.5); }
        case 9u: { return (a1 + a2 - vec4<f32>(0.5)) * 2.0; }
        case 10u: { return a1 - a2; }
        case 11u: { return a1 + a2 - a1 * a2; }
        case 12u: { return mix(a2, a1, diffuse.a); }
        case 13u: { return mix(a2, a1, texel.a); }
        case 14u: { return mix(a2, a1, u.tfactor.a); }
        case 15u: { return a1 + a2 * (1.0 - texel.a); }
        case 16u: { return mix(a2, a1, current.a); }
        case 18u: { return vec4<f32>(a1.rgb + a1.a * a2.rgb, a1.a); }
        case 19u: { return vec4<f32>(a1.rgb * a2.rgb, a1.a); }
        case 20u: { return vec4<f32>(a1.rgb + (1.0 - a1.a) * a2.rgb, a1.a); }
        case 21u: { return vec4<f32>((vec3<f32>(1.0) - a1.rgb) * a2.rgb, a1.a); }
        case 24u: {
            let d = clamp(4.0 * dot(a1.rgb - vec3<f32>(0.5), a2.rgb - vec3<f32>(0.5)), 0.0, 1.0);
            return vec4<f32>(d);
        }
        default: { return a1; }
    }
}

fn compare(func: u32, a: f32, b: f32) -> bool {
    switch (func) {
        case 1u: { return false; }
        case 2u: { return a < b; }
        case 3u: { return a == b; }
        case 4u: { return a <= b; }
        case 5u: { return a > b; }
        case 6u: { return a != b; }
        case 7u: { return a >= b; }
        default: { return true; }
    }
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    let bits = u.flags.x;
    if ((bits & F_CLEAR) != 0u) {
        return i.diffuse;
    }
    var current = i.diffuse;
    var temp = vec4<f32>(0.0);
    let t0 = textureSample(tex0, samp0, i.uv0.xy / i.uv0.z);
    let t1 = textureSample(tex1, samp1, i.uv1.xy / i.uv1.z);
    var keyed = false;
    for (var s = 0u; s < 2u; s = s + 1u) {
        let st = u.stages[s];
        let cop = st.ops.x;
        if (cop <= 1u) { break; }
        var texel = vec4<f32>(1.0);
        if (st.ops2.w != 0u) {
            texel = select(t1, t0, s == 0u);
            if ((bits & F_COLORKEY) != 0u && texel.a == 0.0) {
                keyed = true;
            }
        }
        let c = op(cop, arg(st.ops.y, i.diffuse, current, texel, i.specular, temp), arg(st.ops.z, i.diffuse, current, texel, i.specular, temp), i.diffuse, current, texel);
        var a = current.a;
        let aop = st.ops.w;
        if (aop > 1u) {
            a = op(aop, arg(st.ops2.x, i.diffuse, current, texel, i.specular, temp), arg(st.ops2.y, i.diffuse, current, texel, i.specular, temp), i.diffuse, current, texel).a;
        }
        current = clamp(vec4<f32>(c.rgb, a), vec4<f32>(0.0), vec4<f32>(1.0));
    }
    if (keyed) {
        discard;
    }
    if ((bits & F_SPECULAR_ENABLE) != 0u) {
        current = vec4<f32>(clamp(current.rgb + i.specular.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), current.a);
    }
    if ((bits & F_ALPHATEST) != 0u) {
        let a8 = floor(current.a * 255.0 + 0.5);
        if (!compare(u.flags.y, a8, u.alpha_fog.x)) {
            discard;
        }
    }
    if ((bits & F_FOG) != 0u) {
        var f = 1.0;
        let mode = u.flags.z;
        let d = abs(i.fog_depth.y);
        if (mode == 0u) {
            f = i.fog_depth.x;
        } else if (mode == 1u) {
            f = exp(-u.alpha_fog.w * d);
        } else if (mode == 2u) {
            f = exp(-(u.alpha_fog.w * d) * (u.alpha_fog.w * d));
        } else {
            f = (u.alpha_fog.z - d) / max(u.alpha_fog.z - u.alpha_fog.y, 1e-6);
        }
        f = clamp(f, 0.0, 1.0);
        current = vec4<f32>(mix(u.fog_color.rgb, current.rgb, f), current.a);
    }
    return current;
}
