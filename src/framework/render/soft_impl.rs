use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::common::{Color, Rect};
use crate::framework::backend::{
    BackendIndexBuffer, BackendRenderer, BackendShader, BackendTexture, BackendVertexBuffer,
    BufferUsage, VertexData,
};
use crate::framework::error::{GameError, GameResult};
use crate::framework::graphics::{BlendMode, IndexData};
use crate::framework::render::vertex::VertexDeclaration;

pub struct SoftTexture {
    pub width: u16,
    pub height: u16,
    pub pixels: Rc<RefCell<Vec<u8>>>,
}

impl SoftTexture {
    pub fn new(width: u16, height: u16, data: Option<&[u8]>) -> Self {
        let len = (width as usize) * (height as usize) * 4;
        let mut buf = vec![0u8; len];
        if let Some(src) = data {
            let copy_len = src.len().min(len);
            buf[..copy_len].copy_from_slice(&src[..copy_len]);
        }
        Self {
            width,
            height,
            pixels: Rc::new(RefCell::new(buf)),
        }
    }
}

impl BackendTexture for SoftTexture {
    fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct SoftVertexBuffer {
    count: usize,
}

impl BackendVertexBuffer for SoftVertexBuffer {
    fn set_data_raw(&mut self, _data: &[u8], _offset: usize) -> GameResult {
        Ok(())
    }

    fn vertex_count(&self) -> usize {
        self.count
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct SoftIndexBuffer {
    count: usize,
}

impl BackendIndexBuffer for SoftIndexBuffer {
    fn set_data(&mut self, _data: IndexData, _offset: usize) -> GameResult {
        Ok(())
    }

    fn index_count(&self) -> usize {
        self.count
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct SoftRenderer {
    pub width: u16,
    pub height: u16,
    pub screen_buf: Rc<RefCell<Vec<u8>>>,
    current_target: Option<(u16, u16, Rc<RefCell<Vec<u8>>>)>,
    blend_mode: BlendMode,
    clip_rect: Option<Rect<isize>>,
}

impl SoftRenderer {
    pub fn new(width: u16, height: u16) -> Self {
        let len = (width as usize) * (height as usize) * 4;
        Self {
            width,
            height,
            screen_buf: Rc::new(RefCell::new(vec![0u8; len])),
            current_target: None,
            blend_mode: BlendMode::Alpha,
            clip_rect: None,
        }
    }

    #[inline(always)]
    fn target_info(&self) -> (usize, usize, Rc<RefCell<Vec<u8>>>) {
        if let Some((w, h, ref buf)) = self.current_target {
            (w as usize, h as usize, buf.clone())
        } else {
            (self.width as usize, self.height as usize, self.screen_buf.clone())
        }
    }

    #[inline(always)]
    fn effective_clip(&self, tw: usize, th: usize) -> (isize, isize, isize, isize) {
        if let Some(c) = self.clip_rect {
            let l = c.left.max(0).min(tw as isize);
            let t = c.top.max(0).min(th as isize);
            let r = c.right.max(l).min(tw as isize);
            let b = c.bottom.max(t).min(th as isize);
            (l, t, r, b)
        } else {
            (0, 0, tw as isize, th as isize)
        }
    }

    #[inline(always)]
    fn blend_pixel(dst: &mut [u8], sr: u8, sg: u8, sb: u8, sa: u8, mode: BlendMode) {
        match mode {
            BlendMode::None => {
                dst[0] = sr;
                dst[1] = sg;
                dst[2] = sb;
                dst[3] = sa;
            }
            BlendMode::Alpha => {
                if sa == 0 {
                    return;
                }
                if sa == 255 {
                    dst[0] = sr;
                    dst[1] = sg;
                    dst[2] = sb;
                    dst[3] = 255;
                    return;
                }
                let a = sa as u32;
                let inv_a = 255 - a;
                dst[0] = ((sr as u32 * a + dst[0] as u32 * inv_a) / 255) as u8;
                dst[1] = ((sg as u32 * a + dst[1] as u32 * inv_a) / 255) as u8;
                dst[2] = ((sb as u32 * a + dst[2] as u32 * inv_a) / 255) as u8;
                dst[3] = (a + (dst[3] as u32 * inv_a) / 255).min(255) as u8;
            }
            BlendMode::Add => {
                if sa == 0 {
                    return;
                }
                let a = sa as u32;
                dst[0] = (dst[0] as u32 + (sr as u32 * a) / 255).min(255) as u8;
                dst[1] = (dst[1] as u32 + (sg as u32 * a) / 255).min(255) as u8;
                dst[2] = (dst[2] as u32 + (sb as u32 * a) / 255).min(255) as u8;
            }
            BlendMode::Multiply => {
                dst[0] = ((dst[0] as u32 * sr as u32) / 255) as u8;
                dst[1] = ((dst[1] as u32 * sg as u32) / 255) as u8;
                dst[2] = ((dst[2] as u32 * sb as u32) / 255) as u8;
            }
        }
    }

    #[inline(always)]
    fn is_axis_aligned_quad(v: &[VertexData]) -> bool {
        if v.len() != 6 {
            return false;
        }
        // SpriteBatch::push_quad layout:
        // 0: (left, bottom), 1: (left, top), 2: (right, top)
        // 3: (left, bottom), 4: (right, top), 5: (right, bottom)
        let same_color = v[0].color == v[1].color
            && v[0].color == v[2].color
            && v[0].color == v[5].color;
        if !same_color {
            return false;
        }
        (v[0].position.0 - v[1].position.0).abs() < 1e-3
            && (v[1].position.1 - v[2].position.1).abs() < 1e-3
            && (v[2].position.0 - v[5].position.0).abs() < 1e-3
            && (v[0].position.1 - v[5].position.1).abs() < 1e-3
    }

    fn blit_quad(
        target: &mut [u8],
        tw: usize,
        clip: (isize, isize, isize, isize),
        v: &[VertexData],
        tex: Option<(usize, usize, &[u8])>,
        blend: BlendMode,
    ) {
        let mut x0 = v[1].position.0;
        let mut y0 = v[1].position.1;
        let mut x1 = v[5].position.0;
        let mut y1 = v[5].position.1;
        let mut u0 = v[1].uv.0;
        let mut v0 = v[1].uv.1;
        let mut u1 = v[5].uv.0;
        let mut v1 = v[5].uv.1;

        if x1 < x0 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut u0, &mut u1);
        }
        if y1 < y0 {
            std::mem::swap(&mut y0, &mut y1);
            std::mem::swap(&mut v0, &mut v1);
        }

        let dw = x1 - x0;
        let dh = y1 - y0;
        if dw <= 0.0 || dh <= 0.0 {
            return;
        }

        let ix0 = (x0.floor() as isize).max(clip.0);
        let iy0 = (y0.floor() as isize).max(clip.1);
        let ix1 = (x1.ceil() as isize).min(clip.2);
        let iy1 = (y1.ceil() as isize).min(clip.3);
        if ix1 <= ix0 || iy1 <= iy0 {
            return;
        }

        let (cr, cg, cb, ca) = v[0].color;
        let untinted = cr == 255 && cg == 255 && cb == 255 && ca == 255;

        if let Some((tex_w, tex_h, tex_px)) = tex {
            if tex_w == 0 || tex_h == 0 {
                return;
            }
            let inv_dw = 1.0 / dw;
            let inv_dh = 1.0 / dh;
            let du = u1 - u0;
            let dv = v1 - v0;
            let tw_f = tex_w as f32;
            let th_f = tex_h as f32;
            let max_tx = (tex_w - 1) as isize;
            let max_ty = (tex_h - 1) as isize;

            for py in iy0..iy1 {
                let fy = (py as f32 + 0.5 - y0) * inv_dh;
                if fy < 0.0 || fy >= 1.0 {
                    continue;
                }
                let tv = v0 + fy * dv;
                let sy = ((tv * th_f).floor() as isize).clamp(0, max_ty) as usize;
                let src_row_off = sy * tex_w * 4;
                let dst_row_off = (py as usize) * tw * 4;

                for px in ix0..ix1 {
                    let fx = (px as f32 + 0.5 - x0) * inv_dw;
                    if fx < 0.0 || fx >= 1.0 {
                        continue;
                    }
                    let tu = u0 + fx * du;
                    let sx = ((tu * tw_f).floor() as isize).clamp(0, max_tx) as usize;
                    let s_idx = src_row_off + sx * 4;
                    let mut sr = tex_px[s_idx];
                    let mut sg = tex_px[s_idx + 1];
                    let mut sb = tex_px[s_idx + 2];
                    let mut sa = tex_px[s_idx + 3];

                    if !untinted {
                        sr = ((sr as u32 * cr as u32) / 255) as u8;
                        sg = ((sg as u32 * cg as u32) / 255) as u8;
                        sb = ((sb as u32 * cb as u32) / 255) as u8;
                        sa = ((sa as u32 * ca as u32) / 255) as u8;
                    }

                    let d_idx = dst_row_off + (px as usize) * 4;
                    Self::blend_pixel(&mut target[d_idx..d_idx + 4], sr, sg, sb, sa, blend);
                }
            }
        } else {
            for py in iy0..iy1 {
                let dst_row_off = (py as usize) * tw * 4;
                for px in ix0..ix1 {
                    let d_idx = dst_row_off + (px as usize) * 4;
                    Self::blend_pixel(&mut target[d_idx..d_idx + 4], cr, cg, cb, ca, blend);
                }
            }
        }
    }

    fn raster_triangle(
        target: &mut [u8],
        tw: usize,
        clip: (isize, isize, isize, isize),
        v0: &VertexData,
        v1: &VertexData,
        v2: &VertexData,
        tex: Option<(usize, usize, &[u8])>,
        blend: BlendMode,
    ) {
        let min_x = v0.position.0.min(v1.position.0).min(v2.position.0).floor() as isize;
        let max_x = v0.position.0.max(v1.position.0).max(v2.position.0).ceil() as isize;
        let min_y = v0.position.1.min(v1.position.1).min(v2.position.1).floor() as isize;
        let max_y = v0.position.1.max(v1.position.1).max(v2.position.1).ceil() as isize;

        let ix0 = min_x.max(clip.0);
        let iy0 = min_y.max(clip.1);
        let ix1 = max_x.min(clip.2);
        let iy1 = max_y.min(clip.3);
        if ix1 <= ix0 || iy1 <= iy0 {
            return;
        }

        let (x0, y0) = v0.position;
        let (x1, y1) = v1.position;
        let (x2, y2) = v2.position;

        let denom = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
        if denom.abs() < 1e-6 {
            return;
        }
        let inv_denom = 1.0 / denom;

        for py in iy0..iy1 {
            let fy = py as f32 + 0.5;
            let dst_row_off = (py as usize) * tw * 4;
            for px in ix0..ix1 {
                let fx = px as f32 + 0.5;
                let w0 = ((y1 - y2) * (fx - x2) + (x2 - x1) * (fy - y2)) * inv_denom;
                let w1 = ((y2 - y0) * (fx - x2) + (x0 - x2) * (fy - y2)) * inv_denom;
                let w2 = 1.0 - w0 - w1;

                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }

                let cr = (w0 * v0.color.0 as f32 + w1 * v1.color.0 as f32 + w2 * v2.color.0 as f32)
                    .clamp(0.0, 255.0) as u8;
                let cg = (w0 * v0.color.1 as f32 + w1 * v1.color.1 as f32 + w2 * v2.color.1 as f32)
                    .clamp(0.0, 255.0) as u8;
                let cb = (w0 * v0.color.2 as f32 + w1 * v1.color.2 as f32 + w2 * v2.color.2 as f32)
                    .clamp(0.0, 255.0) as u8;
                let ca = (w0 * v0.color.3 as f32 + w1 * v1.color.3 as f32 + w2 * v2.color.3 as f32)
                    .clamp(0.0, 255.0) as u8;

                let (sr, sg, sb, sa) = if let Some((tex_w, tex_h, tex_px)) = tex {
                    if tex_w == 0 || tex_h == 0 {
                        continue;
                    }
                    let u = w0 * v0.uv.0 + w1 * v1.uv.0 + w2 * v2.uv.0;
                    let v = w0 * v0.uv.1 + w1 * v1.uv.1 + w2 * v2.uv.1;
                    let sx = ((u * tex_w as f32).floor() as isize).clamp(0, (tex_w - 1) as isize) as usize;
                    let sy = ((v * tex_h as f32).floor() as isize).clamp(0, (tex_h - 1) as isize) as usize;
                    let s_idx = (sy * tex_w + sx) * 4;
                    (
                        ((tex_px[s_idx] as u32 * cr as u32) / 255) as u8,
                        ((tex_px[s_idx + 1] as u32 * cg as u32) / 255) as u8,
                        ((tex_px[s_idx + 2] as u32 * cb as u32) / 255) as u8,
                        ((tex_px[s_idx + 3] as u32 * ca as u32) / 255) as u8,
                    )
                } else {
                    (cr, cg, cb, ca)
                };

                let d_idx = dst_row_off + (px as usize) * 4;
                Self::blend_pixel(&mut target[d_idx..d_idx + 4], sr, sg, sb, sa, blend);
            }
        }
    }
}

impl BackendRenderer for SoftRenderer {
    fn renderer_name(&self) -> String {
        "MPC Software 320x240".to_owned()
    }

    fn clear(&mut self, color: Color) {
        let (r, g, b, a) = color.to_rgba();
        let (_, _, target_rc) = self.target_info();
        let mut buf = target_rc.borrow_mut();
        for chunk in buf.chunks_exact_mut(4) {
            chunk[0] = r;
            chunk[1] = g;
            chunk[2] = b;
            chunk[3] = a;
        }
    }

    fn present(&mut self) -> GameResult {
        Ok(())
    }

    fn create_texture_mutable(&mut self, width: u16, height: u16) -> GameResult<Box<dyn BackendTexture>> {
        Ok(Box::new(SoftTexture::new(width, height, None)))
    }

    fn create_texture(&mut self, width: u16, height: u16, data: &[u8]) -> GameResult<Box<dyn BackendTexture>> {
        Ok(Box::new(SoftTexture::new(width, height, Some(data))))
    }

    fn set_blend_mode(&mut self, blend: BlendMode) -> GameResult {
        self.blend_mode = blend;
        Ok(())
    }

    fn set_render_target(&mut self, texture: Option<&Box<dyn BackendTexture>>) -> GameResult {
        if let Some(tex) = texture {
            let soft_tex = tex
                .as_any()
                .downcast_ref::<SoftTexture>()
                .ok_or_else(|| GameError::RenderError("Invalid texture type for SoftRenderer".to_owned()))?;
            self.current_target = Some((soft_tex.width, soft_tex.height, soft_tex.pixels.clone()));
        } else {
            self.current_target = None;
        }
        Ok(())
    }

    fn draw_rect(&mut self, rect: Rect<isize>, color: Color) -> GameResult {
        let (tw, th, target_rc) = self.target_info();
        let clip = self.effective_clip(tw, th);
        let ix0 = rect.left.max(clip.0);
        let iy0 = rect.top.max(clip.1);
        let ix1 = rect.right.min(clip.2);
        let iy1 = rect.bottom.min(clip.3);
        if ix1 <= ix0 || iy1 <= iy0 {
            return Ok(());
        }

        let (r, g, b, a) = color.to_rgba();
        let blend = self.blend_mode;
        let mut target = target_rc.borrow_mut();

        for py in iy0..iy1 {
            let row_off = (py as usize) * tw * 4;
            for px in ix0..ix1 {
                let d_idx = row_off + (px as usize) * 4;
                Self::blend_pixel(&mut target[d_idx..d_idx + 4], r, g, b, a, blend);
            }
        }
        Ok(())
    }

    fn draw_outline_rect(&mut self, rect: Rect<isize>, line_width: usize, color: Color) -> GameResult {
        if line_width == 0 {
            return Ok(());
        }
        let lw = line_width as isize;
        self.draw_rect(Rect::new(rect.left, rect.top, rect.right, rect.top + lw), color)?;
        self.draw_rect(Rect::new(rect.left, rect.bottom - lw, rect.right, rect.bottom), color)?;
        self.draw_rect(Rect::new(rect.left, rect.top + lw, rect.left + lw, rect.bottom - lw), color)?;
        self.draw_rect(Rect::new(rect.right - lw, rect.top + lw, rect.right, rect.bottom - lw), color)?;
        Ok(())
    }

    fn set_clip_rect(&mut self, rect: Option<Rect>) -> GameResult {
        self.clip_rect = rect;
        Ok(())
    }

    fn draw_triangles(
        &mut self,
        vertices: &[VertexData],
        texture: Option<&Box<dyn BackendTexture>>,
        shader: BackendShader,
    ) -> GameResult {
        if vertices.len() < 3 {
            return Ok(());
        }

        let (tw, th, target_rc) = self.target_info();
        let clip = self.effective_clip(tw, th);
        let blend = self.blend_mode;

        let use_tex = matches!(shader, BackendShader::Texture);
        let soft_tex_opt = if use_tex {
            if let Some(tex) = texture {
                Some(
                    tex.as_any()
                        .downcast_ref::<SoftTexture>()
                        .ok_or_else(|| GameError::RenderError("Invalid texture type".to_owned()))?,
                )
            } else {
                None
            }
        } else {
            None
        };

        // Handle case where source texture and render target might share Rc (unlikely, but safe)
        if let Some(st) = soft_tex_opt {
            if Rc::ptr_eq(&st.pixels, &target_rc) {
                return Ok(());
            }
        }

        let mut target = target_rc.borrow_mut();
        let tex_borrow = soft_tex_opt.map(|st| (st.width as usize, st.height as usize, st.pixels.borrow()));
        let tex_slice = tex_borrow.as_ref().map(|(w, h, b)| (*w, *h, b.as_slice()));

        if vertices.len() % 6 == 0 {
            let mut idx = 0;
            while idx + 6 <= vertices.len() {
                let quad = &vertices[idx..idx + 6];
                if Self::is_axis_aligned_quad(quad) {
                    Self::blit_quad(&mut target, tw, clip, quad, tex_slice, blend);
                } else {
                    Self::raster_triangle(&mut target, tw, clip, &quad[0], &quad[1], &quad[2], tex_slice, blend);
                    Self::raster_triangle(&mut target, tw, clip, &quad[3], &quad[4], &quad[5], tex_slice, blend);
                }
                idx += 6;
            }
        } else {
            for tri in vertices.chunks_exact(3) {
                Self::raster_triangle(&mut target, tw, clip, &tri[0], &tri[1], &tri[2], tex_slice, blend);
            }
        }

        Ok(())
    }

    fn draw_triangles_indexed(
        &mut self,
        vertices: &[VertexData],
        indices: IndexData,
        texture: Option<&Box<dyn BackendTexture>>,
        shader: BackendShader,
    ) -> GameResult {
        let (tw, th, target_rc) = self.target_info();
        let clip = self.effective_clip(tw, th);
        let blend = self.blend_mode;

        let use_tex = matches!(shader, BackendShader::Texture);
        let soft_tex_opt = if use_tex {
            if let Some(tex) = texture {
                Some(
                    tex.as_any()
                        .downcast_ref::<SoftTexture>()
                        .ok_or_else(|| GameError::RenderError("Invalid texture type".to_owned()))?,
                )
            } else {
                None
            }
        } else {
            None
        };

        if let Some(st) = soft_tex_opt {
            if Rc::ptr_eq(&st.pixels, &target_rc) {
                return Ok(());
            }
        }

        let mut target = target_rc.borrow_mut();
        let tex_borrow = soft_tex_opt.map(|st| (st.width as usize, st.height as usize, st.pixels.borrow()));
        let tex_slice = tex_borrow.as_ref().map(|(w, h, b)| (*w, *h, b.as_slice()));

        match indices {
            IndexData::UByte(idx) => {
                for tri in idx.chunks_exact(3) {
                    let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                    if i0 < vertices.len() && i1 < vertices.len() && i2 < vertices.len() {
                        Self::raster_triangle(
                            &mut target,
                            tw,
                            clip,
                            &vertices[i0],
                            &vertices[i1],
                            &vertices[i2],
                            tex_slice,
                            blend,
                        );
                    }
                }
            }
            IndexData::UShort(idx) => {
                for tri in idx.chunks_exact(3) {
                    let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                    if i0 < vertices.len() && i1 < vertices.len() && i2 < vertices.len() {
                        Self::raster_triangle(
                            &mut target,
                            tw,
                            clip,
                            &vertices[i0],
                            &vertices[i1],
                            &vertices[i2],
                            tex_slice,
                            blend,
                        );
                    }
                }
            }
            IndexData::UInt(idx) => {
                for tri in idx.chunks_exact(3) {
                    let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                    if i0 < vertices.len() && i1 < vertices.len() && i2 < vertices.len() {
                        Self::raster_triangle(
                            &mut target,
                            tw,
                            clip,
                            &vertices[i0],
                            &vertices[i1],
                            &vertices[i2],
                            tex_slice,
                            blend,
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn create_vertex_buffer(
        &mut self,
        _decl: VertexDeclaration,
        count: usize,
        _usage: BufferUsage,
    ) -> GameResult<Box<dyn BackendVertexBuffer>> {
        Ok(Box::new(SoftVertexBuffer { count }))
    }

    fn create_index_buffer(&mut self, count: usize, _usage: BufferUsage) -> GameResult<Box<dyn BackendIndexBuffer>> {
        Ok(Box::new(SoftIndexBuffer { count }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

