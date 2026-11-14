use mux_core::application_event::{CanvasTransform, Point, ScreenState};

/// CSS content rectangle (the example canvas has no border or padding). A fresh
/// value is read for every pointer event and repaint, including scroll and zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasGeometry {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub pixel_ratio: f64,
}

impl CanvasGeometry {
    pub fn backing_size(self) -> Result<[u32; 2], &'static str> {
        if [
            self.left,
            self.top,
            self.width,
            self.height,
            self.pixel_ratio,
        ]
        .iter()
        .any(|value| !value.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
            || self.pixel_ratio <= 0.0
            || self.pixel_ratio > 16.0
        {
            return Err("Canvas is hidden or its geometry is invalid");
        }
        let width = (self.width * self.pixel_ratio).round();
        let height = (self.height * self.pixel_ratio).round();
        if !(1.0..=16384.0).contains(&width)
            || !(1.0..=16384.0).contains(&height)
            || width * height > 32_000_000.0
        {
            return Err("Canvas backing extent exceeds the bounded renderer");
        }
        Ok([width as u32, height as u32])
    }

    pub fn transform(self, screens: &ScreenState) -> Result<CanvasTransform, &'static str> {
        self.backing_size()?;
        screens.validate().map_err(|_| "Invalid screen revision")?;
        Ok(CanvasTransform {
            revision: screens.revision,
            screen: screens.desktop,
            width: self.width,
            height: self.height,
        })
    }

    /// Browser clientX/Y are CSS viewport coordinates, never desktop pixels.
    pub fn to_screen(
        self,
        client: [f64; 2],
        screens: &ScreenState,
        revision: u64,
    ) -> Result<Point, &'static str> {
        let transform = self.transform(screens)?;
        let local = [client[0] - self.left, client[1] - self.top];
        if local.iter().any(|value| !value.is_finite())
            || local[0] < 0.0
            || local[1] < 0.0
            || local[0] >= self.width
            || local[1] >= self.height
        {
            return Err("Pointer is outside the canvas");
        }
        let mut point = transform
            .to_screen(local, revision)
            .map_err(|_| "Stale or invalid canvas transform")?;
        // Rounding near the right/bottom edge must stay in the half-open desktop.
        point.x = point
            .x
            .clamp(screens.desktop.left, screens.desktop.right - 1);
        point.y = point
            .y
            .clamp(screens.desktop.top, screens.desktop.bottom - 1);
        Ok(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::desktop::Bounds;

    fn screens() -> ScreenState {
        let desktop = Bounds {
            left: -1920,
            top: -100,
            right: 1920,
            bottom: 2060,
        };
        ScreenState {
            revision: 9,
            desktop,
            screens: vec![desktop],
        }
    }

    #[test]
    fn negative_desktop_css_offset_and_backing_are_independent() {
        let geometry = CanvasGeometry {
            left: 40.0,
            top: 80.0,
            width: 960.0,
            height: 540.0,
            pixel_ratio: 2.0,
        };
        assert_eq!(geometry.backing_size(), Ok([1920, 1080]));
        assert_eq!(
            geometry.to_screen([520.0, 350.0], &screens(), 9),
            Ok(Point { x: 0, y: 980 })
        );
        assert_eq!(
            geometry
                .transform(&screens())
                .unwrap()
                .to_canvas(Point { x: 0, y: 980 }, 9)
                .unwrap(),
            [480.0, 270.0]
        );
        assert!(geometry.to_screen([520.0, 350.0], &screens(), 8).is_err());
    }

    #[test]
    fn resize_zoom_bounds_and_hidden_canvas_fail_closed() {
        let mut geometry = CanvasGeometry {
            left: -20.0,
            top: 0.0,
            width: 100.0,
            height: 100.0,
            pixel_ratio: 1.25,
        };
        assert_eq!(geometry.backing_size(), Ok([125, 125]));
        assert_eq!(
            geometry.to_screen([79.999, 99.999], &screens(), 9),
            Ok(Point { x: 1919, y: 2059 })
        );
        assert!(geometry.to_screen([80.0, 50.0], &screens(), 9).is_err());
        geometry.width = 0.0;
        assert!(geometry.backing_size().is_err());
        geometry.width = f64::NAN;
        assert!(geometry.backing_size().is_err());
    }
}
