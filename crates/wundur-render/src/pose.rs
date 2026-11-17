//! A model put in a pose (its rest pose, or a moment of one of its animations): every
//! triangle in world space, skinned, ready to draw.

use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

use crate::gltf::{Interp, Model, Path3};

pub struct Scene {
    pub pos: Vec<Vec3>,
    pub nrm: Vec<Vec3>,
    pub uv0: Vec<Vec2>,
    pub uv1: Vec<Vec2>,
    pub col: Vec<Vec4>,
    pub tris: Vec<[u32; 3]>,
    /// Each triangle's material (`None`: glTF's default one).
    pub tri_mat: Vec<Option<u32>>,
    /// Triangles with no normals of their own: shaded flat.
    pub tri_flat: Vec<bool>,
    pub min: Vec3,
    pub max: Vec3,
    /// Every node's place in the world, in this pose.
    pub world: Vec<Mat4>,
}

impl Scene {
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Turn everything `angle` radians about +Y (through the origin).
    pub fn turn(&mut self, angle: f32) {
        let q = Quat::from_rotation_y(angle);
        let m = Mat4::from_quat(q);
        for p in &mut self.pos {
            *p = q * *p;
        }
        for n in &mut self.nrm {
            *n = q * *n;
        }
        for w in &mut self.world {
            *w = m * *w;
        }
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &self.pos {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        if !self.pos.is_empty() {
            (self.min, self.max) = (lo, hi);
        }
    }

    pub fn radius(&self) -> f32 {
        ((self.max - self.min).length() * 0.5).max(1e-6)
    }
}

/// `model` in its rest pose, or at `time` seconds into animation `anim`.
pub fn pose(model: &Model, anim: Option<(usize, f32)>) -> Scene {
    let n = model.nodes.len();
    let mut trs: Vec<(Vec3, Quat, Vec3)> = model.nodes.iter().map(|nd| (nd.translation, nd.rotation, nd.scale)).collect();
    let mut animated = vec![false; n];
    if let Some((a, t)) = anim
        && let Some(a) = model.animations.get(a)
    {
        for ch in &a.channels {
            let Some(v) = sample(ch.interp, &ch.times, &ch.values, if ch.path == Path3::Rotation { 4 } else { 3 }, t) else { continue };
            let slot = &mut trs[ch.node];
            match ch.path {
                Path3::Translation => slot.0 = Vec3::new(v[0], v[1], v[2]),
                Path3::Scale => slot.2 = Vec3::new(v[0], v[1], v[2]),
                Path3::Rotation => slot.1 = Quat::from_xyzw(v[0], v[1], v[2], v[3]).normalize(),
            }
            animated[ch.node] = true;
        }
    }
    let local = |i: usize| -> Mat4 {
        let nd = &model.nodes[i];
        match nd.matrix {
            Some(m) if !animated[i] => m,
            _ => Mat4::from_scale_rotation_translation(trs[i].2, trs[i].1, trs[i].0),
        }
    };
    let mut world = vec![Mat4::IDENTITY; n];
    let mut seen = vec![false; n];
    let mut stack: Vec<(usize, Mat4)> = model.roots.iter().rev().map(|&r| (r, Mat4::IDENTITY)).collect();
    let mut order = Vec::new();
    while let Some((i, parent)) = stack.pop() {
        if seen[i] {
            continue;
        }
        seen[i] = true;
        world[i] = parent * local(i);
        order.push(i);
        for &c in model.nodes[i].children.iter().rev() {
            if c < n {
                stack.push((c, world[i]));
            }
        }
    }

    let mut s = Scene { pos: vec![], nrm: vec![], uv0: vec![], uv1: vec![], col: vec![], tris: vec![], tri_mat: vec![], tri_flat: vec![], min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN), world: world.clone() };
    for &i in &order {
        let node = &model.nodes[i];
        let Some(mesh) = node.mesh.and_then(|m| model.meshes.get(m)) else { continue };
        let skin = node.skin.and_then(|k| model.skins.get(k));
        let joint_mats: Option<Vec<Mat4>> = skin.map(|sk| sk.joints.iter().enumerate().map(|(j, &node)| world[node] * sk.inverse_bind.get(j).copied().unwrap_or(Mat4::IDENTITY)).collect());
        let m = world[i];
        let nm = Mat3::from_mat4(m).inverse().transpose();
        let flip = m.determinant() < 0.0;
        for p in &mesh.prims {
            let base = s.pos.len() as u32;
            let skinned = joint_mats.as_ref().filter(|_| p.joints.is_some() && p.weights.is_some());
            for (v, &pos) in p.positions.iter().enumerate() {
                let nrm = p.normals.as_ref().and_then(|ns| ns.get(v)).copied().unwrap_or(Vec3::Y);
                let (wp, wn) = match skinned {
                    Some(jm) => {
                        let (js, ws) = (p.joints.as_ref().unwrap()[v], p.weights.as_ref().unwrap().get(v).copied().unwrap_or(Vec4::ZERO));
                        let mut acc = Mat4::ZERO;
                        let mut total = 0.0;
                        for k in 0..4 {
                            let w = ws[k];
                            if w > 0.0
                                && let Some(j) = jm.get(js[k] as usize)
                            {
                                acc += *j * w;
                                total += w;
                            }
                        }
                        if total <= 1e-6 {
                            (m.transform_point3(pos), nm * nrm)
                        } else {
                            let acc = acc * (1.0 / total);
                            (acc.transform_point3(pos), Mat3::from_mat4(acc).inverse().transpose() * nrm)
                        }
                    }
                    None => (m.transform_point3(pos), nm * nrm),
                };
                s.min = s.min.min(wp);
                s.max = s.max.max(wp);
                s.pos.push(wp);
                s.nrm.push(wn.normalize_or_zero());
                s.uv0.push(p.uv0.as_ref().and_then(|u| u.get(v)).copied().unwrap_or(Vec2::ZERO));
                s.uv1.push(p.uv1.as_ref().and_then(|u| u.get(v)).copied().unwrap_or(Vec2::ZERO));
                s.col.push(p.colors.as_ref().and_then(|c| c.get(v)).copied().unwrap_or(Vec4::ONE));
            }
            for t in p.indices.chunks_exact(3) {
                let tri = if flip { [base + t[0], base + t[2], base + t[1]] } else { [base + t[0], base + t[1], base + t[2]] };
                s.tris.push(tri);
                s.tri_mat.push(p.material.map(|m| m as u32));
                s.tri_flat.push(p.normals.is_none());
            }
        }
    }
    if s.pos.is_empty() {
        s.min = Vec3::splat(-0.5);
        s.max = Vec3::splat(0.5);
    }
    s
}

/// A channel's value at `t` (clamped to its keys).
fn sample(interp: Interp, times: &[f32], values: &[f32], n: usize, t: f32) -> Option<Vec<f32>> {
    let keys = times.len();
    let per = if interp == Interp::Cubic { n * 3 } else { n };
    if keys == 0 || values.len() < keys * per {
        return None;
    }
    let at = |k: usize, part: usize| -> &[f32] {
        let o = k * per + if interp == Interp::Cubic { part * n } else { 0 };
        &values[o..o + n]
    };
    if t <= times[0] || keys == 1 {
        return Some(at(0, 1).to_vec());
    }
    if t >= times[keys - 1] {
        return Some(at(keys - 1, 1).to_vec());
    }
    let k = times.partition_point(|&x| x <= t).saturating_sub(1).min(keys - 2);
    let (t0, t1) = (times[k], times[k + 1]);
    let dt = (t1 - t0).max(1e-6);
    let u = ((t - t0) / dt).clamp(0.0, 1.0);
    let out: Vec<f32> = match interp {
        Interp::Step => at(k, 1).to_vec(),
        Interp::Linear if n == 4 => {
            let (a, b) = (at(k, 0), at(k + 1, 0));
            let q = Quat::from_xyzw(a[0], a[1], a[2], a[3]).normalize().slerp(Quat::from_xyzw(b[0], b[1], b[2], b[3]).normalize(), u);
            vec![q.x, q.y, q.z, q.w]
        }
        Interp::Linear => at(k, 0).iter().zip(at(k + 1, 0)).map(|(a, b)| a + (b - a) * u).collect(),
        Interp::Cubic => {
            let (u2, u3) = (u * u, u * u * u);
            let (v0, o0, v1, i1) = (at(k, 1), at(k, 2), at(k + 1, 1), at(k + 1, 0));
            (0..n).map(|c| (2.0 * u3 - 3.0 * u2 + 1.0) * v0[c] + (u3 - 2.0 * u2 + u) * dt * o0[c] + (-2.0 * u3 + 3.0 * u2) * v1[c] + (u3 - u2) * dt * i1[c]).collect()
        }
    };
    Some(out)
}
