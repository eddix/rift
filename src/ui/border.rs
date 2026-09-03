use objc2::rc::Retained;
use objc2_app_kit::NSColor;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_quartz_core::CALayer;
use tracing::warn;

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
    movement_grouped: bool,
    movement_tracking_verified: bool,
    ordering_grouped: bool,
}

impl FocusBorderWindow {
    pub fn new(target: BorderTarget, style: BorderStyle) -> Result<Self, CgsWindowError> {
        let frame = surface_frame(target.frame, style.width);
        let root_layer = CALayer::layer();
        configure_layer(&root_layer, frame.size, style);

        let cgs_window = CgsWindow::new_overlay(frame)?;
        cgs_window.set_opacity(false)?;
        cgs_window.disable_shadow()?;
        cgs_window.set_alpha(0.0)?;
        cgs_window.set_resolution(style.resolution())?;
        cgs_window.set_shape_regions(frame.origin, &rounded_ring_regions(frame.size, style))?;
        cgs_window.move_to_space(target.space);
        let context = cgs_window.create_context()?;

        let mut window = Self {
            target: target.window_server_id,
            frame,
            style,
            space: target.space,
            root_layer,
            cgs_window,
            context,
            movement_grouped: false,
            movement_tracking_verified: false,
            ordering_grouped: false,
        };
        window.redraw()?;
        window.sync_geometry_and_order()?;
        window.attach_target_groups();
        window.sync_order()?;
        window.cgs_window.set_alpha(1.0)?;
        Ok(window)
    }

    pub fn update(
        &mut self,
        target: BorderTarget,
        style: BorderStyle,
    ) -> Result<(), CgsWindowError> {
        debug_assert_eq!(self.target, target.window_server_id);
        let next_frame = surface_frame(target.frame, style.width);
        let size_changed = self.frame.size != next_frame.size;
        let origin_changed = self.frame.origin != next_frame.origin;
        let style_changed = self.style != style;
        let resolution_changed = self.style.hidpi != style.hidpi;
        let shape_changed = size_changed
            || self.style.width != style.width
            || self.style.corner_radius != style.corner_radius
            || resolution_changed;

        if self.space != target.space {
            self.cgs_window.move_to_space(target.space);
            self.space = target.space;
            self.movement_tracking_verified = false;
        }

        if size_changed || style_changed {
            self.cgs_window.set_alpha(0.0)?;
            self.cgs_window.disable_updates()?;
            let update_result = (|| {
                self.cgs_window.freeze()?;
                if shape_changed {
                    self.cgs_window.set_shape_regions(
                        next_frame.origin,
                        &rounded_ring_regions(next_frame.size, style),
                    )?;
                } else if origin_changed && !self.movement_grouped {
                    self.cgs_window.move_to(next_frame.origin)?;
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
            self.cgs_window.set_alpha(1.0)?;
        } else if origin_changed {
            self.frame.origin = next_frame.origin;
            if self.movement_grouped && !self.movement_tracking_verified {
                self.verify_movement_tracking(next_frame.origin);
            }
            if !self.movement_grouped {
                self.cgs_window.move_to(next_frame.origin)?;
            }
        }

        if size_changed || style_changed {
            self.sync_order()?;
        }
        Ok(())
    }

    fn redraw(&self) -> Result<(), CgsWindowError> {
        render_layer_to_context(&self.context, self.frame.size, &self.root_layer);
        self.cgs_window.flush_content()
    }

    pub fn targets(&self, target: BorderTarget) -> bool { self.target == target.window_server_id }

    pub fn should_resync_order_for(&self, window: WindowServerId) -> bool {
        !self.ordering_grouped && self.target == window
    }

    fn attach_target_groups(&mut self) {
        self.movement_grouped = match self.cgs_window.add_to_movement_group(self.target.as_u32()) {
            Ok(()) => true,
            Err(error) => {
                warn!(?error, target = ?self.target, "movement-group attachment unavailable; using coalesced frame fallback");
                false
            }
        };
        self.ordering_grouped = match self.cgs_window.add_to_ordering_group(self.target.as_u32()) {
            Ok(()) => true,
            Err(error) => {
                warn!(?error, target = ?self.target, "ordering-group attachment unavailable; retaining relative-order fallback");
                false
            }
        };
    }

    fn verify_movement_tracking(&mut self, expected_origin: CGPoint) {
        let actual_origin = window_server::get_window(WindowServerId::new(self.cgs_window.id()))
            .map(|window| window.frame.origin);
        let tracks_target = actual_origin.is_some_and(|actual| {
            (actual.x - expected_origin.x).abs() <= 1.0
                && (actual.y - expected_origin.y).abs() <= 1.0
        });
        if tracks_target {
            self.movement_tracking_verified = true;
        } else {
            warn!(
                target = ?self.target,
                border = self.cgs_window.id(),
                ?actual_origin,
                ?expected_origin,
                "movement group did not carry the border; enabling frame fallback"
            );
            self.movement_grouped = false;
        }
    }

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
            .sync_above(self.target.as_u32(), self.frame.origin, level, sub_level)?;
        self.reinforce_event_passthrough()
    }

    pub fn sync_order(&self) -> Result<(), CgsWindowError> {
        let (level, sub_level) = self.target_level()?;
        self.cgs_window.sync_order_above(self.target.as_u32(), level, sub_level)?;
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

fn surface_frame(target: CGRect, width: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(target.origin.x - width, target.origin.y - width),
        CGSize::new(target.size.width + width * 2.0, target.size.height + width * 2.0),
    )
}

fn rounded_ring_regions(size: CGSize, style: BorderStyle) -> Vec<CGRect> {
    let resolution = style.resolution().max(1.0);
    let row_height = 1.0 / resolution;
    let outer_radius = (style.corner_radius + style.width)
        .max(0.0)
        .min(size.width / 2.0)
        .min(size.height / 2.0);
    // Keep one physical pixel beyond the CALayer stroke in the WindowServer
    // shape so antialiased edge pixels are not clipped.
    let inner_inset = (style.width + row_height).max(0.0);
    let inner_size = CGSize::new(
        (size.width - inner_inset * 2.0).max(0.0),
        (size.height - inner_inset * 2.0).max(0.0),
    );
    let inner_radius = (outer_radius - inner_inset)
        .max(0.0)
        .min(inner_size.width / 2.0)
        .min(inner_size.height / 2.0);

    let row_count = (size.height / row_height).ceil() as usize;
    let mut regions = Vec::with_capacity(row_count.saturating_mul(2));
    for row in 0..row_count {
        let y = row as f64 * row_height;
        let height = row_height.min(size.height - y);
        if height <= 0.0 {
            continue;
        }
        let sample_y = y + height / 2.0;
        let Some((outer_left, outer_right)) = rounded_rect_span(size, outer_radius, sample_y)
        else {
            continue;
        };
        let inner_span = rounded_rect_span(inner_size, inner_radius, sample_y - inner_inset)
            .map(|(left, right)| (left + inner_inset, right + inner_inset));

        if let Some((inner_left, inner_right)) = inner_span {
            push_region(&mut regions, outer_left, y, inner_left - outer_left, height);
            push_region(&mut regions, inner_right, y, outer_right - inner_right, height);
        } else {
            push_region(&mut regions, outer_left, y, outer_right - outer_left, height);
        }
    }
    regions
}

fn rounded_rect_span(size: CGSize, radius: f64, y: f64) -> Option<(f64, f64)> {
    if y < 0.0 || y >= size.height || size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let radius = radius.max(0.0).min(size.width / 2.0).min(size.height / 2.0);
    if radius == 0.0 {
        return Some((0.0, size.width));
    }
    let distance_from_center = if y < radius {
        radius - y
    } else if y > size.height - radius {
        y - (size.height - radius)
    } else {
        0.0
    };
    let inset = if distance_from_center == 0.0 {
        0.0
    } else {
        radius - (radius * radius - distance_from_center * distance_from_center).max(0.0).sqrt()
    };
    Some((inset, size.width - inset))
}

fn push_region(regions: &mut Vec<CGRect>, x: f64, y: f64, width: f64, height: f64) {
    if width > 0.0 && height > 0.0 {
        regions.push(CGRect::new(CGPoint::new(x, y), CGSize::new(width, height)));
    }
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

    #[test]
    fn rounded_ring_shape_should_exclude_window_center() {
        let style = BorderStyle {
            width: 3.0,
            color: 0xff00_e5ff,
            corner_radius: 18.0,
            hidpi: true,
        };
        let size = CGSize::new(808.0, 608.0);
        let regions = rounded_ring_regions(size, style);

        assert!(!regions_contain(
            &regions,
            CGPoint::new(size.width / 2.0, size.height / 2.0)
        ));
    }

    #[test]
    fn rounded_ring_shape_should_keep_diagonal_corner_stroke() {
        let style = BorderStyle {
            width: 3.0,
            color: 0xff00_e5ff,
            corner_radius: 18.0,
            hidpi: true,
        };
        let outer_radius = style.corner_radius + style.width;
        let stroke_radius = outer_radius - style.width / 2.0;
        let diagonal = stroke_radius / 2.0_f64.sqrt();
        let point = CGPoint::new(outer_radius - diagonal, outer_radius - diagonal);
        let regions = rounded_ring_regions(CGSize::new(808.0, 608.0), style);

        assert!(
            regions_contain(&regions, point),
            "rounded corner point {point:?} was clipped"
        );
    }

    #[test]
    fn rounded_ring_shape_should_only_emit_positive_regions() {
        let style = BorderStyle {
            width: 3.0,
            color: 0xff00_e5ff,
            corner_radius: 18.0,
            hidpi: true,
        };
        let regions = rounded_ring_regions(CGSize::new(808.0, 608.0), style);

        assert!(regions.iter().all(|region| region.size.width > 0.0 && region.size.height > 0.0));
    }

    fn regions_contain(regions: &[CGRect], point: CGPoint) -> bool {
        regions.iter().any(|region| {
            point.x >= region.origin.x
                && point.x < region.origin.x + region.size.width
                && point.y >= region.origin.y
                && point.y < region.origin.y + region.size.height
        })
    }
}
