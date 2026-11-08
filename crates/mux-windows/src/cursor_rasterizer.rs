//! Owner-thread GPU artwork. Only cursor source assets are uploaded; the final
//! shapes, text, badges, scaling and halo are rendered into the DXGI backbuffer.
use crate::{app_cursor::AppCursorImage, overlay::RenderRecipe};
use anyhow::{Context, Result};
use mux_core::desktop::{BadgeShape, CursorShape};
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Dxgi::{Common::*, IDXGIDevice, IDXGISurface},
    },
    core::w,
};

macro_rules! point {
    ($x:expr, $y:expr) => {{
        let mut p = D2D1_ELLIPSE::default().point;
        p.X = $x as f32;
        p.Y = $y as f32;
        p
    }};
}

pub(crate) struct Rasterizer {
    _thread: PhantomData<Rc<()>>,
    factory: ID2D1Factory1,
    context: ID2D1DeviceContext,
    text: IDWriteFactory,
    font: Option<(u32, IDWriteTextFormat)>,
    // At most two native/custom cursor assets, independent of final canvas size.
    images: Vec<(u64, u32, u32, ID2D1Bitmap1)>,
    pub(crate) asset_uploads: u64,
}

impl Rasterizer {
    pub(crate) fn new(device: &IDXGIDevice) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let context = factory
                .CreateDevice(device)?
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            context.SetUnitMode(D2D1_UNIT_MODE_PIXELS);
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Self {
                _thread: PhantomData,
                factory,
                context,
                text: DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)?,
                font: None,
                images: Vec::new(),
                asset_uploads: 0,
            })
        }
    }

    fn image(&mut self, image: &AppCursorImage) -> Result<ID2D1Bitmap1> {
        if let Some(index) = self.images.iter().position(|(id, w, h, _)| {
            *id == image.identity && *w == image.width && *h == image.height
        }) {
            let cached = self.images.remove(index);
            let bitmap = cached.3.clone();
            self.images.push(cached);
            return Ok(bitmap);
        }
        anyhow::ensure!(
            image.width > 0
                && image.height > 0
                && image.width <= 512
                && image.height <= 512
                && image.pixels.len() == image.width as usize * image.height as usize * 4,
            "invalid cursor asset"
        );
        let bitmap = unsafe {
            self.context.CreateBitmap(
                D2D_SIZE_U {
                    width: image.width,
                    height: image.height,
                },
                Some(image.pixels.as_ptr().cast()),
                image.width * 4,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.,
                    dpiY: 96.,
                    ..Default::default()
                },
            )?
        };
        if self.images.len() == 2 {
            self.images.remove(0);
        }
        self.images
            .push((image.identity, image.width, image.height, bitmap.clone()));
        self.asset_uploads += 1;
        Ok(bitmap)
    }

    pub(crate) fn draw(&mut self, surface: &IDXGISurface, recipe: &RenderRecipe) -> Result<()> {
        unsafe {
            let target = self.context.CreateBitmapFromDxgiSurface(
                surface,
                Some(&D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.,
                    dpiY: 96.,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    ..Default::default()
                }),
            )?;
            self.context.SetTarget(&target);
            self.context.BeginDraw();
            self.context.Clear(Some(&D2D1_COLOR_F::default()));
            let result = self.artwork(recipe);
            // Always EndDraw and unbind even if creating an asset/brush failed.
            let end = self.context.EndDraw(None, None);
            self.context.SetTarget(None);
            drop(target);
            result?;
            end.context("finish GPU cursor rasterization")?;
        }
        Ok(())
    }

    fn artwork(&mut self, recipe: &RenderRecipe) -> Result<()> {
        let settings = &recipe.settings;
        let layout = &recipe.layout;
        let rgb = if recipe.paused {
            [148, 163, 184]
        } else {
            settings.color
        };
        let color = D2D1_COLOR_F {
            r: rgb[0] as f32 / 255.,
            g: rgb[1] as f32 / 255.,
            b: rgb[2] as f32 / 255.,
            a: 1.,
        };
        let white = D2D1_COLOR_F {
            r: 1.,
            g: 1.,
            b: 1.,
            a: 1.,
        };
        let (hx, hy) = layout.hotspot;
        let size = settings.size.round();
        unsafe {
            let fill = self.context.CreateSolidColorBrush(&color, None)?;
            let edge = self.context.CreateSolidColorBrush(&white, None)?;
            if let Some(app) = recipe.application.as_ref() {
                if app.visible == Some(true) {
                    // A stock observation carries an identifier, not source pixels.
                    // Extract only that shared glyph; final composition stays GPU.
                    let stock = app
                        .image
                        .is_none()
                        .then(|| crate::system_cursor::raster(app.kind, size as i32));
                    let image = app
                        .image
                        .as_ref()
                        .or_else(|| stock.as_ref().and_then(|source| source.image.as_ref()))
                        .context("visible cursor has no GPU source glyph")?;
                    let bitmap = self.image(image)?;
                    let (width, height, ihx, ihy) = if app.image.is_some() {
                        crate::overlay::image_geometry(image, size as i32)
                    } else {
                        // Keep the established stock illustration rectangle/hotspot.
                        let (x, y) = crate::overlay::stock_hotspot(app.kind, size as i32);
                        (size as i32, size as i32, x, y)
                    };
                    let x = (hx - ihx) as f32;
                    let y = (hy - ihy) as f32;
                    if settings.shape == CursorShape::System {
                        let shadow = self.context.CreateEffect(&CLSID_D2D1Shadow)?;
                        shadow.SetInput(0, &bitmap, true);
                        let scale = (width as f32 / image.width as f32)
                            .max(height as f32 / image.height as f32);
                        let deviation = (1.0 / scale).clamp(0.0039, 250.0);
                        shadow.SetValue(
                            D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                            D2D1_PROPERTY_TYPE_FLOAT,
                            &deviation.to_le_bytes(),
                        )?;
                        let rgba = [color.r, color.g, color.b, 0.8f32]
                            .into_iter()
                            .flat_map(f32::to_le_bytes)
                            .collect::<Vec<_>>();
                        shadow.SetValue(
                            D2D1_SHADOW_PROP_COLOR.0 as u32,
                            D2D1_PROPERTY_TYPE_VECTOR4,
                            &rgba,
                        )?;
                        let mut matrix = D2D1_BRUSH_PROPERTIES::default().transform;
                        matrix.M11 = width as f32 / image.width as f32;
                        matrix.M22 = height as f32 / image.height as f32;
                        matrix.M31 = x;
                        matrix.M32 = y;
                        self.context.SetTransform(&matrix);
                        self.context.DrawImage(
                            &shadow.GetOutput()?,
                            None,
                            None,
                            D2D1_INTERPOLATION_MODE_LINEAR,
                            D2D1_COMPOSITE_MODE_SOURCE_OVER,
                        );
                        matrix.M11 = 1.;
                        matrix.M22 = 1.;
                        matrix.M31 = 0.;
                        matrix.M32 = 0.;
                        self.context.SetTransform(&matrix);
                    }
                    self.context.DrawBitmap(
                        &bitmap,
                        Some(&D2D_RECT_F {
                            left: x,
                            top: y,
                            right: x + width as f32,
                            bottom: y + height as f32,
                        }),
                        1.,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                }
            } else {
                let (x, y) = (hx as f32, hy as f32);
                match settings.shape {
                    CursorShape::Arrow | CursorShape::System => {
                        let geometry = self.factory.CreatePathGeometry()?;
                        let sink = geometry.Open()?;
                        let points = [
                            (1., 1.),
                            (1., size),
                            (size * 0.28, size * 0.72),
                            (size * 0.52, size * 1.12),
                            (size * 0.7, size * 1.02),
                            (size * 0.47, size * 0.62),
                            (size * 0.83, size * 0.62),
                        ]
                        .map(|(px, py)| point!(x + px, y + py));
                        sink.BeginFigure(points[0], D2D1_FIGURE_BEGIN_FILLED);
                        sink.AddLines(&points[1..]);
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        sink.Close()?;
                        self.context.FillGeometry(&geometry, &fill, None);
                        self.context.DrawGeometry(&geometry, &edge, 2., None);
                    }
                    CursorShape::Crosshair | CursorShape::IBeam => {
                        let half = size / 2.;
                        self.context.DrawLine(
                            point!(x, y - half),
                            point!(x, y + half),
                            &fill,
                            2.,
                            None,
                        );
                        if settings.shape == CursorShape::Crosshair {
                            self.context.DrawLine(
                                point!(x - half, y),
                                point!(x + half, y),
                                &fill,
                                2.,
                                None,
                            );
                            self.context.FillEllipse(
                                &D2D1_ELLIPSE {
                                    point: point!(x, y),
                                    radiusX: 2.,
                                    radiusY: 2.,
                                },
                                &fill,
                            );
                        } else {
                            let half_width = half.min(7.);
                            for py in [y - half, y + half] {
                                self.context.DrawLine(
                                    point!(x - half_width, py),
                                    point!(x + half_width, py),
                                    &fill,
                                    2.,
                                    None,
                                );
                            }
                        }
                    }
                    CursorShape::Ring => self.context.DrawEllipse(
                        &D2D1_ELLIPSE {
                            point: point!(x, y),
                            radiusX: size / 2.,
                            radiusY: size / 2.,
                        },
                        &fill,
                        2.,
                        None,
                    ),
                }
            }
            if self
                .font
                .as_ref()
                .is_none_or(|(size, _)| *size != settings.label_font_size.to_bits())
            {
                let format = self.text.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    settings.label_font_size,
                    w!(""),
                )?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                self.font = Some((settings.label_font_size.to_bits(), format));
            }
            let font = &self.font.as_ref().context("missing cursor font")?.1;
            if let Some(rect) = layout.name {
                self.context.DrawText(
                    &layout.rendered_label.encode_utf16().collect::<Vec<_>>(),
                    font,
                    &rect.gpu(),
                    &fill,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_GDI_CLASSIC,
                );
            }
            if let (Some(rect), Some(text), Some(badge)) =
                (layout.badge, layout.text, settings.badge.as_ref())
            {
                let r = rect.gpu();
                match settings.badge_shape {
                    BadgeShape::Rounded => {
                        let rounded = D2D1_ROUNDED_RECT {
                            rect: r,
                            radiusX: 4.,
                            radiusY: 4.,
                        };
                        self.context.FillRoundedRectangle(&rounded, &fill);
                        self.context.DrawRoundedRectangle(&rounded, &edge, 1., None);
                    }
                    BadgeShape::Box => {
                        self.context.FillRectangle(&r, &fill);
                        self.context.DrawRectangle(&r, &edge, 1., None);
                    }
                    BadgeShape::Circle => {
                        let ellipse = D2D1_ELLIPSE {
                            point: point!((r.left + r.right) / 2., (r.top + r.bottom) / 2.),
                            radiusX: (r.right - r.left) / 2.,
                            radiusY: (r.bottom - r.top) / 2.,
                        };
                        self.context.FillEllipse(&ellipse, &fill);
                        self.context.DrawEllipse(&ellipse, &edge, 1., None);
                    }
                }
                let text_brush = if rgb.iter().map(|c| u32::from(*c)).sum::<u32>() > 3 * 127 {
                    self.context.CreateSolidColorBrush(
                        &D2D1_COLOR_F {
                            r: 0.063,
                            g: 0.063,
                            b: 0.063,
                            a: 1.,
                        },
                        None,
                    )?
                } else {
                    edge
                };
                self.context.DrawText(
                    &badge.encode_utf16().collect::<Vec<_>>(),
                    font,
                    &text.gpu(),
                    &text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_GDI_CLASSIC,
                );
            }
        }
        Ok(())
    }
}
