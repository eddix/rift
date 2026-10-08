use objc2::rc::Retained;
use objc2_app_kit::NSColor;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_quartz_core::CALayer;
use tracing::warn;

use crate::common::config::BorderSettings;
use crate::model::projection::BorderTarget;
use crate::sys::cgs_window::{CgsWindow, CgsWindowError, CgsWindowMotionHandle};
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

    pub fn for_target(settings: BorderSettings, target: BorderTarget) -> Self {
        let corner_radius = if settings.adaptive_corner_radius {
            target.corner_radius.unwrap_or(settings.corner_radius)
        } else {
            settings.corner_radius
        };
        Self {
            width: settings.width,
            color: settings.color,
            corner_radius,
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
        let frame = border_frame(target.frame, style.width);
        let root_layer = CALayer::layer();
        configure_layer(&root_layer, frame.size, style);

        let cgs_window = CgsWindow::new_overlay(frame)?;
        cgs_window.set_opacity(false)?;
        cgs_window.disable_shadow()?;
        cgs_window.set_alpha(0.0)?;
        cgs_window.set_resolution(style.resolution())?;
        cgs_window.set_shape(frame)?;
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
        window.sync_geometry_and_order()?;
        window.cgs_window.set_alpha(1.0)?;
        Ok(window)
    }

    pub fn update(
        &mut self,
        target: BorderTarget,
        style: BorderStyle,
    ) -> Result<(), CgsWindowError> {
        debug_assert_eq!(self.target, target.window_server_id);
        let next_frame = border_frame(target.frame, style.width);
        let size_changed = self.frame.size != next_frame.size;
        let origin_changed = self.frame.origin != next_frame.origin;
        let style_changed = self.style != style;
        let resolution_changed = self.style.hidpi != style.hidpi;

        if self.space != target.space {
            self.cgs_window.move_to_space(target.space);
            self.space = target.space;
        }

        if size_changed || style_changed {
            self.cgs_window.disable_updates()?;
            let update_result = (|| {
                self.cgs_window.freeze()?;
                if size_changed {
                    self.cgs_window.set_shape(next_frame)?;
                } else if origin_changed {
                    self.cgs_window.move_with_group(next_frame.origin)?;
                }
                self.frame = next_frame;
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
        } else if origin_changed {
            self.frame.origin = next_frame.origin;
            self.cgs_window.move_with_group(next_frame.origin)?;
        }

        if size_changed || style_changed {
            self.sync_geometry_and_order()?;
        }
        Ok(())
    }

    fn redraw(&self) -> Result<(), CgsWindowError> {
        render_layer_to_context(&self.context, self.frame.size, &self.root_layer);
        self.cgs_window.flush_content()
    }

    pub fn targets(&self, target: BorderTarget) -> bool { self.target == target.window_server_id }

    pub(crate) fn motion_handle(&self) -> CgsWindowMotionHandle { self.cgs_window.motion_handle() }

    pub fn should_resync_order_for(&self, window: WindowServerId) -> bool { self.target == window }

    fn target_level(&self) -> Result<(i32, i32), CgsWindowError> {
        let level = window_server::window_level(self.target.as_u32())
            .ok_or(CgsWindowError::Level(objc2_core_graphics::CGError(1000)))?;
        let level = i32::try_from(level)
            .map_err(|_| CgsWindowError::Level(objc2_core_graphics::CGError(1000)))?;
        Ok((level, window_server::window_sub_level(self.target.as_u32())))
    }

    fn sync_geometry_and_order(&self) -> Result<(), CgsWindowError> {
        let (level, sub_level) = self.target_level()?;
        self.cgs_window
            .sync_below(self.target.as_u32(), self.frame.origin, level, sub_level)?;
        self.reinforce_event_passthrough()
    }

    pub fn sync_order(&self) -> Result<(), CgsWindowError> {
        let (level, sub_level) = self.target_level()?;
        self.cgs_window.sync_order_below(self.target.as_u32(), level, sub_level)?;
        self.reinforce_event_passthrough()
    }

    fn reinforce_event_passthrough(&self) -> Result<(), CgsWindowError> {
        self.cgs_window.set_tags(border_window_tags().bits())?;
        self.cgs_window.clear_tags(SLSWindowTags::OPAQUE_FOR_EVENTS.bits())?;
        self.observe_event_passthrough();
        Ok(())
    }

    fn observe_event_passthrough(&self) {
        let Some(tags) = window_server::window_tags(self.cgs_window.id()) else {
            return;
        };
        if !tags.contains(SLSWindowTags::IGNORE_FOR_EVENTS)
            || tags.contains(SLSWindowTags::OPAQUE_FOR_EVENTS)
        {
            warn!(
                border = self.cgs_window.id(),
                ?tags,
                "WindowServer returned transient event-routing tags after a successful update"
            );
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

pub(crate) fn border_origin(target: CGRect, width: f64) -> CGPoint {
    CGPoint::new(target.origin.x - width, target.origin.y - width)
}

pub(crate) fn border_frame(target: CGRect, width: f64) -> CGRect {
    CGRect::new(
        border_origin(target, width),
        CGSize::new(target.size.width + width * 2.0, target.size.height + width * 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::app::WindowId;

    fn target_with_corner_radius(corner_radius: Option<f64>) -> BorderTarget {
        BorderTarget {
            window: WindowId::new(1, 1),
            window_server_id: WindowServerId::new(1),
            space: SpaceId::new(1),
            frame: CGRect::new(CGPoint::ZERO, CGSize::new(800.0, 600.0)),
            corner_radius,
        }
    }

    #[test]
    fn surface_frame_should_keep_border_outside_target_content() {
        let target = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(800.0, 600.0));

        assert_eq!(
            border_frame(target, 4.0),
            CGRect::new(CGPoint::new(96.0, 196.0), CGSize::new(808.0, 608.0))
        );
    }

    #[test]
    fn border_origin_should_match_the_surface_frame_origin() {
        let target = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(800.0, 600.0));

        assert_eq!(border_origin(target, 4.0), border_frame(target, 4.0).origin);
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

    #[test]
    fn adaptive_border_style_uses_the_window_server_corner_radius() {
        let style = BorderStyle::for_target(
            BorderSettings::default(),
            target_with_corner_radius(Some(13.0)),
        );

        assert_eq!(style.corner_radius, 13.0);
    }

    #[test]
    fn adaptive_border_style_falls_back_to_the_configured_radius() {
        let settings = BorderSettings {
            corner_radius: 10.0,
            ..BorderSettings::default()
        };

        let style = BorderStyle::for_target(settings, target_with_corner_radius(None));

        assert_eq!(style.corner_radius, 10.0);
    }

    #[test]
    fn fixed_border_style_ignores_the_window_server_corner_radius() {
        let settings = BorderSettings {
            corner_radius: 14.0,
            adaptive_corner_radius: false,
            ..BorderSettings::default()
        };

        let style = BorderStyle::for_target(settings, target_with_corner_radius(Some(13.0)));

        assert_eq!(style.corner_radius, 14.0);
    }
}
