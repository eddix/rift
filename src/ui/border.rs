use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSNormalWindowLevel};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_quartz_core::CALayer;

use crate::common::config::BorderSettings;
use crate::model::projection::BorderTarget;
use crate::sys::cgs_window::{CgsWindow, CgsWindowError};
use crate::sys::window_server::WindowServerId;
use crate::ui::common::{render_layer_to_cgs_window, with_disabled_actions};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderStyle {
    pub width: f64,
    pub color: u32,
    pub corner_radius: f64,
    pub hidpi: bool,
}

impl BorderStyle {
    fn contents_scale(self) -> f64 { if self.hidpi { 2.0 } else { 1.0 } }
}

impl From<BorderSettings> for BorderStyle {
    fn from(settings: BorderSettings) -> Self {
        Self {
            width: settings.width,
            color: settings.color,
            corner_radius: settings.corner_radius,
            hidpi: settings.hidpi,
        }
    }
}

pub struct FocusBorderWindow {
    target: WindowServerId,
    frame: CGRect,
    style: BorderStyle,
    root_layer: Retained<CALayer>,
    cgs_window: CgsWindow,
}

impl FocusBorderWindow {
    pub fn new(target: BorderTarget, style: BorderStyle) -> Result<Self, CgsWindowError> {
        let frame = surface_frame(target.frame, style.width);
        let root_layer = CALayer::layer();
        root_layer.setFrame(local_frame(frame.size));
        root_layer.setContentsScale(style.contents_scale());
        root_layer.setOpaque(false);

        let cgs_window = CgsWindow::new(frame)?;
        cgs_window.set_opacity(false)?;
        cgs_window.set_alpha(1.0)?;
        cgs_window.set_level(NSNormalWindowLevel as i32)?;
        cgs_window.set_tags(1 << 3)?;

        let window = Self {
            target: target.window_server_id,
            frame,
            style,
            root_layer,
            cgs_window,
        };
        window.redraw();
        window.order_above_target()?;
        Ok(window)
    }

    pub fn update(
        &mut self,
        target: BorderTarget,
        style: BorderStyle,
    ) -> Result<(), CgsWindowError> {
        let next_frame = surface_frame(target.frame, style.width);
        let size_changed = self.frame.size != next_frame.size;
        let frame_changed = self.frame != next_frame;
        let style_changed = self.style != style;

        if frame_changed {
            self.cgs_window.set_shape(next_frame)?;
            self.frame = next_frame;
        }
        if style_changed {
            self.root_layer.setContentsScale(style.contents_scale());
            self.style = style;
        }
        if size_changed || style_changed {
            self.root_layer.setFrame(local_frame(next_frame.size));
            self.redraw();
        }

        self.target = target.window_server_id;
        self.order_above_target()
    }

    fn redraw(&self) {
        let (red, green, blue, alpha) = argb_components(self.style.color);
        let color = NSColor::colorWithRed_green_blue_alpha(red, green, blue, alpha);
        let clear = NSColor::clearColor();
        let bounds = local_frame(self.frame.size);
        with_disabled_actions(|| {
            self.root_layer.setFrame(bounds);
            self.root_layer.setOpaque(false);
            self.root_layer.setBackgroundColor(Some(&clear.CGColor()));
            self.root_layer.setBorderWidth(self.style.width);
            self.root_layer.setBorderColor(Some(&color.CGColor()));
            self.root_layer.setCornerRadius(self.style.corner_radius + self.style.width);
        });
        render_layer_to_cgs_window(self.cgs_window.id(), self.frame.size, &self.root_layer);
    }

    fn order_above_target(&self) -> Result<(), CgsWindowError> {
        self.cgs_window.order_above(Some(self.target.as_u32()))
    }
}

fn local_frame(size: CGSize) -> CGRect { CGRect::new(CGPoint::ZERO, size) }

fn argb_components(color: u32) -> (f64, f64, f64, f64) {
    let component = |shift: u32| f64::from((color >> shift) & 0xff_u32) / 255.0;
    (component(16), component(8), component(0), component(24))
}

fn surface_frame(target: CGRect, width: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(target.origin.x - width, target.origin.y - width),
        CGSize::new(target.size.width + width * 2.0, target.size.height + width * 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_frame_should_keep_border_outside_target_content() {
        let target = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(800.0, 600.0));

        assert_eq!(
            surface_frame(target, 4.0),
            CGRect::new(CGPoint::new(96.0, 196.0), CGSize::new(808.0, 608.0))
        );
    }

    #[test]
    fn argb_components_should_decode_janky_borders_color_format() {
        let (red, green, blue, alpha) = argb_components(0xfff3_7021);

        assert_eq!(
            (red * 255.0, green * 255.0, blue * 255.0, alpha),
            (243.0, 112.0, 33.0, 1.0)
        );
    }
}
