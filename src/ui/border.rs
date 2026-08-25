use objc2::rc::Retained;
use objc2_app_kit::NSColor;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_quartz_core::CALayer;

use crate::common::config::BorderSettings;
use crate::model::projection::BorderTarget;
use crate::sys::cgs_window::{CgsWindow, CgsWindowError};
use crate::sys::screen::SpaceId;
use crate::sys::skylight::SLSWindowTags;
use crate::sys::window_server::{self, WindowServerId};
use crate::ui::common::{render_layer_to_context, with_disabled_actions};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderStyle {
    pub width: f64,
    pub color: u32,
    pub corner_radius: f64,
    pub hidpi: bool,
}

impl BorderStyle {
    fn resolution(self) -> f64 { if self.hidpi { 2.0 } else { 1.0 } }
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
    space: SpaceId,
    root_layer: Retained<CALayer>,
    cgs_window: CgsWindow,
    context: CFRetained<CGContext>,
}

impl FocusBorderWindow {
    pub fn new(target: BorderTarget, style: BorderStyle) -> Result<Self, CgsWindowError> {
        let frame = surface_frame(target.frame, style.width);
        let root_layer = CALayer::layer();
        configure_layer(&root_layer, frame.size, style);

        let cgs_window = CgsWindow::new_overlay(frame)?;
        cgs_window.set_tags(border_window_tags().bits())?;
        cgs_window.clear_tags(0)?;
        cgs_window.set_opacity(false)?;
        cgs_window.disable_shadow()?;
        cgs_window.set_alpha(0.0)?;
        cgs_window.set_resolution(style.resolution())?;
        cgs_window.move_to_space(target.space);
        let context = cgs_window.create_context()?;

        let window = Self {
            target: target.window_server_id,
            frame,
            style,
            space: target.space,
            root_layer,
            cgs_window,
            context,
        };
        window.redraw()?;
        window.sync_order()?;
        window.verify_event_passthrough()?;
        window.cgs_window.set_alpha(1.0)?;
        Ok(window)
    }

    pub fn update(
        &mut self,
        target: BorderTarget,
        style: BorderStyle,
    ) -> Result<(), CgsWindowError> {
        let next_frame = surface_frame(target.frame, style.width);
        let size_changed = self.frame.size != next_frame.size;
        let origin_changed = self.frame.origin != next_frame.origin;
        let style_changed = self.style != style;
        let resolution_changed = self.style.hidpi != style.hidpi;

        if self.space != target.space {
            self.cgs_window.move_to_space(target.space);
            self.space = target.space;
        }

        if size_changed || style_changed {
            self.cgs_window.set_alpha(0.0)?;
            self.cgs_window.disable_updates()?;
            let update_result = (|| {
                self.cgs_window.freeze()?;
                if size_changed {
                    self.cgs_window.set_shape(next_frame)?;
                    self.frame = next_frame;
                } else if origin_changed {
                    self.frame.origin = next_frame.origin;
                }
                if resolution_changed {
                    self.cgs_window.set_resolution(style.resolution())?;
                    self.context = self.cgs_window.create_context()?;
                }
                self.style = style;
                configure_layer(&self.root_layer, self.frame.size, style);
                self.redraw()?;
                self.cgs_window.thaw()
            })();
            if update_result.is_err() {
                let _ = self.cgs_window.thaw();
            }
            let reenable_result = self.cgs_window.reenable_updates();
            update_result?;
            reenable_result?;
            self.cgs_window.set_alpha(1.0)?;
        } else if origin_changed {
            self.frame.origin = next_frame.origin;
        }

        self.target = target.window_server_id;
        self.sync_order()
    }

    fn redraw(&self) -> Result<(), CgsWindowError> {
        render_layer_to_context(&self.context, self.frame.size, &self.root_layer);
        self.cgs_window.flush_content()
    }

    pub fn sync_order(&self) -> Result<(), CgsWindowError> {
        let level = window_server::window_level(self.target.as_u32())
            .ok_or(CgsWindowError::Level(objc2_core_graphics::CGError(1000)))?;
        let level = i32::try_from(level)
            .map_err(|_| CgsWindowError::Level(objc2_core_graphics::CGError(1000)))?;
        let sub_level = window_server::window_sub_level(self.target.as_u32());
        self.cgs_window.sync_above(
            self.target.as_u32(),
            self.frame.origin,
            level,
            sub_level,
        )
    }

    fn verify_event_passthrough(&self) -> Result<(), CgsWindowError> {
        let tags = window_server::window_tags(self.cgs_window.id())
            .ok_or(CgsWindowError::Events(objc2_core_graphics::CGError(1000)))?;
        if tags.contains(SLSWindowTags::IGNORE_FOR_EVENTS)
            && !tags.contains(SLSWindowTags::OPAQUE_FOR_EVENTS)
        {
            Ok(())
        } else {
            Err(CgsWindowError::Events(objc2_core_graphics::CGError(1000)))
        }
    }
}

fn configure_layer(layer: &CALayer, size: CGSize, style: BorderStyle) {
    let (red, green, blue, alpha) = argb_components(style.color);
    let color = NSColor::colorWithRed_green_blue_alpha(red, green, blue, alpha);
    let clear = NSColor::clearColor();
    with_disabled_actions(|| {
        layer.setFrame(CGRect::new(CGPoint::ZERO, size));
        layer.setContentsScale(style.resolution());
        layer.setOpaque(false);
        layer.setBackgroundColor(Some(&clear.CGColor()));
        layer.setBorderWidth(style.width);
        layer.setBorderColor(Some(&color.CGColor()));
        layer.setCornerRadius(style.corner_radius + style.width);
    });
}

fn border_window_tags() -> SLSWindowTags {
    SLSWindowTags::FLOATING | SLSWindowTags::IGNORE_FOR_EVENTS
}

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

    #[test]
    fn overlay_tags_should_pass_pointer_events_through() {
        let tags = border_window_tags();
        assert!(tags.contains(SLSWindowTags::FLOATING));
        assert!(tags.contains(SLSWindowTags::IGNORE_FOR_EVENTS));
        assert!(!tags.contains(SLSWindowTags::OPAQUE_FOR_EVENTS));
    }
}
