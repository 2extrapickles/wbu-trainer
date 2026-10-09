//! Edits on `\x01SQX` meshes: s16 positions /1024, 11:11:10 normals, and a
//! skinning layout where a vertex's reference picks a slot in its group's palette, the slot names a
//! weight record, and the record holds up to 4 (bone, weight/32) pairs.
//!
//! Every function works on a standalone SQX blob (the mesh starts at offset 0).

pub struct Layout {
    pub ngroups: usize,
    pub nweights: usize,
    pub nidx: usize,
    pub nvert: usize,
    pub groups: usize,
    pub palette: usize,
    pub weights: usize,
    pub indices: usize,
    pub verts: usize,
    pub size: usize,
}

fn u16_at(d: &[u8], o: usize) -> usize {
    u16::from_le_bytes([d[o], d[o + 1]]) as usize
}

fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn set_f32(d: &mut [u8], o: usize, v: f32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Layout from the header counts; `d` needs at least the 0x38-byte header.
pub fn layout(d: &[u8]) -> Option<Layout> {
    if d.len() < 0x38 || &d[..4] != b"\x01SQX" {
        return None;
    }
    let (ngroups, npal, nweights, nidx, nvert) = (u16_at(d, 0x14), u16_at(d, 0x16), u16_at(d, 0x18), u16_at(d, 0x1a), u16_at(d, 0x1c));
    let groups = 0x38;
    let palette = groups + ngroups * 12;
    let weights = palette + npal * 2;
    let indices = weights + nweights * 8;
    let verts = indices + nidx * 2;
    Some(Layout { ngroups, nweights, nidx, nvert, groups, palette, weights, indices, verts, size: verts + nvert * 16 })
}

/// The first SQX mesh in a model file (player.bin / boat.bin / object.bin), as its own blob.
pub fn extract(file: &[u8]) -> Option<Vec<u8>> {
    let o = file.windows(4).position(|w| w == b"\x01SQX")?;
    let l = layout(&file[o..])?;
    file.get(o..o + l.size).map(<[u8]>::to_vec)
}

pub fn pos(d: &[u8], l: &Layout, i: usize) -> [f32; 3] {
    let o = l.verts + i * 16;
    [0, 1, 2].map(|k| i16::from_le_bytes([d[o + k * 2], d[o + k * 2 + 1]]) as f32 / 1024.0)
}

fn set_pos(d: &mut [u8], l: &Layout, i: usize, p: [f32; 3]) {
    let o = l.verts + i * 16;
    for (k, c) in p.iter().enumerate() {
        let raw = (c * 1024.0).round().clamp(-32768.0, 32767.0) as i16;
        d[o + k * 2..o + k * 2 + 2].copy_from_slice(&raw.to_le_bytes());
    }
}

/// (bone, weight) pairs of a weight record.
fn record(d: &[u8], l: &Layout, r: usize) -> Vec<(u8, f32)> {
    let rec = &d[l.weights + r * 8..l.weights + r * 8 + 8];
    (0..4).filter(|&k| rec[k] != 0xFF && rec[4 + k] > 0).map(|k| (rec[k], rec[4 + k] as f32 / 32.0)).collect()
}

/// Calls `f(group, vertex, weight record)` once per vertex, through the group palettes.
fn for_vertices(d: &[u8], l: &Layout, mut f: impl FnMut(usize, usize, usize)) {
    let mut seen = vec![false; l.nvert];
    for g in 0..l.ngroups {
        let r = l.groups + g * 12;
        let (nbones, pal_start, count, start) = (d[r + 1] as usize, u16_at(d, r + 4), u16_at(d, r + 6), u16_at(d, r + 8));
        for k in start..(start + count).min(l.nidx) {
            let v = u16_at(d, l.indices + k * 2);
            if v >= l.nvert || seen[v] {
                continue;
            }
            seen[v] = true;
            let slot = u16_at(d, l.verts + v * 16 + 6) / 3;
            if slot < nbones.max(1) {
                let rec = u16_at(d, l.palette + (pal_start + slot) * 2);
                if rec < l.nweights {
                    f(g, v, rec);
                }
            }
        }
    }
}

fn vertex_weights(d: &[u8], l: &Layout) -> Vec<Vec<(u8, f32)>> {
    let mut out = vec![Vec::new(); l.nvert];
    for_vertices(d, l, |_, v, rec| out[v] = record(d, l, rec));
    out
}

/// Per bone: (bone, centroid of the vertices it mostly drives, vertex count).
pub fn bone_centroids(d: &[u8], l: &Layout) -> Vec<(u8, [f32; 3], usize)> {
    let mut acc: Vec<(u8, [f32; 3], usize)> = Vec::new();
    for (i, ws) in vertex_weights(d, l).iter().enumerate() {
        if let Some(&(b, _)) = ws.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
            let p = pos(d, l, i);
            match acc.iter_mut().find(|a| a.0 == b) {
                Some(a) => {
                    (0..3).for_each(|k| a.1[k] += p[k]);
                    a.2 += 1;
                }
                None => acc.push((b, p, 1)),
            }
        }
    }
    for a in &mut acc {
        a.1 = a.1.map(|c| c / a.2 as f32);
    }
    acc
}

/// The bone a rigid replacement follows: the wakeboard's bone (the group drawn only with variant
/// bit 9) when there is one, else the bone that drives most vertices.
pub fn attach_bone(d: &[u8], l: &Layout) -> u8 {
    let mut board: Vec<u8> = Vec::new();
    for_vertices(d, l, |g, _, rec| {
        if d[l.groups + g * 12 + 2] == 9
            && let Some(&(b, _)) = record(d, l, rec).iter().max_by(|a, b| a.1.total_cmp(&b.1))
        {
            board.push(b);
        }
    });
    let pick = |v: &[u8]| (0..=255u8).max_by_key(|b| v.iter().filter(|x| *x == b).count());
    if !board.is_empty() {
        return pick(&board).unwrap_or(0);
    }
    bone_centroids(d, l).iter().max_by_key(|c| c.2).map_or(0, |c| c.0)
}

/// Bind-pose bounding box (min, max).
pub fn bbox(d: &[u8], l: &Layout) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for i in 0..l.nvert {
        let p = pos(d, l, i);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// Rewrite every bone reference with `map`.
pub fn remap_bones(d: &mut [u8], l: &Layout, map: impl Fn(u8) -> u8) {
    for r in 0..l.nweights {
        for k in 0..4 {
            let o = l.weights + r * 8 + k;
            if d[o] != 0xFF {
                d[o] = map(d[o]);
            }
        }
    }
}

/// Bind every vertex fully to one bone (weight 32 is the game's rigid fast path).
pub fn make_rigid(d: &mut [u8], l: &Layout, bone: u8) {
    for r in 0..l.nweights {
        d[l.weights + r * 8..l.weights + r * 8 + 8].copy_from_slice(&[bone, 0xFF, 0xFF, 0xFF, 32, 0, 0, 0]);
    }
}

/// Draw every group regardless of the variant mask (board / jet ski / helmet variants).
pub fn clear_variants(d: &mut [u8], l: &Layout) {
    for g in 0..l.ngroups {
        d[l.groups + g * 12 + 2] = 0;
    }
}

/// Rotate around Y by `quarter_turns` x 90 degrees, scale by `s`, then translate by `t`.
/// Normals are rotated with the mesh.
pub fn transform(d: &mut [u8], l: &Layout, quarter_turns: u32, s: f32, t: [f32; 3]) {
    let rot = |p: [f32; 3]| match quarter_turns % 4 {
        0 => p,
        1 => [p[2], p[1], -p[0]],
        2 => [-p[0], p[1], -p[2]],
        _ => [-p[2], p[1], p[0]],
    };
    for i in 0..l.nvert {
        let p = rot(pos(d, l, i));
        set_pos(d, l, i, [0, 1, 2].map(|k| p[k] * s + t[k]));
        let o = l.verts + i * 16 + 8;
        let n = u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let field = |v: u32, bits: u32| {
            let m = 1i32 << (bits - 1);
            ((v & ((1 << bits) - 1)) as i32 ^ m) - m
        };
        let (nx, ny, nz) = (field(n, 11) as f32 / 1023.0, field(n >> 11, 11) as f32 / 1023.0, field(n >> 22, 10) as f32 / 511.0);
        let r = rot([nx, ny, nz]);
        let pack = |v: f32, scale: f32, bits: u32| ((v * scale).round() as i32 as u32) & ((1 << bits) - 1);
        let packed = pack(r[0], 1023.0, 11) | pack(r[1], 1023.0, 11) << 11 | pack(r[2], 511.0, 10) << 22;
        d[o..o + 4].copy_from_slice(&packed.to_le_bytes());
    }
    refit_sphere(d, l);
}

/// Recompute the culling sphere (centre at the box centre, radius covering every vertex).
pub fn refit_sphere(d: &mut [u8], l: &Layout) {
    let (lo, hi) = bbox(d, l);
    let c = [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0);
    let r = (0..l.nvert)
        .map(|i| {
            let p = pos(d, l, i);
            ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2)).sqrt()
        })
        .fold(0.0f32, f32::max);
    for k in 0..3 {
        set_f32(d, 4 + k * 4, c[k]);
    }
    set_f32(d, 0x10, r.max(f32_at(d, 0x10)) * 1.2);
}

/// Scale a rider's head around the base of the skull. The head is the bone whose strongly
/// weighted vertices sit highest in the bind pose (bone numbering differs per rider). Vertices are
/// scaled in proportion to their head weight, so the neck blends smoothly. Returns new positions.
pub fn big_head(d: &[u8], scale: f32) -> Option<Vec<[f32; 3]>> {
    let l = layout(d)?;
    let weights = vertex_weights(d, &l);
    let mut stats: Vec<(u8, usize, f32)> = Vec::new();
    for (i, ws) in weights.iter().enumerate() {
        for &(b, w) in ws {
            if w >= 0.5 {
                let y = pos(d, &l, i)[1];
                match stats.iter_mut().find(|s| s.0 == b) {
                    Some(s) => {
                        s.1 += 1;
                        s.2 += y;
                    }
                    None => stats.push((b, 1, y)),
                }
            }
        }
    }
    let head = stats.iter().filter(|s| s.1 >= 50).max_by(|a, b| (a.2 / a.1 as f32).total_cmp(&(b.2 / b.1 as f32)))?.0;
    let core: Vec<[f32; 3]> =
        (0..l.nvert).filter(|&i| weights[i].iter().any(|&(b, w)| b == head && w >= 0.9)).map(|i| pos(d, &l, i)).collect();
    let n = core.len().max(1) as f32;
    let pivot = [
        core.iter().map(|p| p[0]).sum::<f32>() / n,
        core.iter().map(|p| p[1]).fold(f32::MAX, f32::min),
        core.iter().map(|p| p[2]).sum::<f32>() / n,
    ];
    Some(
        (0..l.nvert)
            .map(|i| {
                let k: f32 = weights[i].iter().filter(|w| w.0 == head).map(|w| w.1).sum();
                let f = 1.0 + (scale - 1.0) * k;
                let p = pos(d, &l, i);
                [0, 1, 2].map(|a| pivot[a] + (p[a] - pivot[a]) * f)
            })
            .collect(),
    )
}

/// Raw s16 xyz for writing straight into the game's vertex array.
pub fn encode(p: [f32; 3]) -> [i16; 3] {
    p.map(|c| (c * 1024.0).round().clamp(-32768.0, 32767.0) as i16)
}
