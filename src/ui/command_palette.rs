//! Lightweight AppKit command-palette panel.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBorderType, NSColor, NSControl, NSControlTextEditingDelegate, NSEvent,
    NSFocusRingType, NSFont, NSFontAttributeName, NSFontWeightMedium,
    NSForegroundColorAttributeName, NSGraphicsContext, NSImage, NSPanel, NSPopUpMenuWindowLevel,
    NSRunningApplication, NSScreen, NSScrollView, NSStringDrawing, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing, NSTextField, NSTextFieldDelegate, NSTextView, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColorSpace, CGContext, CGGradient, CGGradientDrawingOptions};
use objc2_foundation::{
    MainThreadMarker, NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSNotification,
    NSNumber, NSObject, NSObjectProtocol, NSString,
};
use objc2_quartz_core::{
    CABasicAnimation, CAMediaTiming, CAMediaTimingFunction, kCAMediaTimingFunctionEaseInEaseOut,
};

use crate::model::command_palette::PaletteEntryKind;
use crate::sys::app::NSRunningApplicationExt;
use crate::sys::screen::NSScreenExt;
use crate::sys::window_server::{self, WindowServerId};

const SEARCH_FIELD_HEIGHT: f64 = 28.0;
const SEARCH_RESULTS_GAP: f64 = 9.0;
const HEADER_CHROME_HORIZONTAL_INSET: f64 = 8.0;
const HEADER_CHROME_VERTICAL_PADDING: f64 = 4.0;
const ROW_HEIGHT: f64 = 44.0;
const PANEL_PADDING: f64 = 12.0;
const RESULTS_BOTTOM_PADDING: f64 = 10.0;
const TOP_PADDING: f64 = 11.0;
const ICON_SIZE: f64 = 27.0;
const MIN_TITLE_WIDTH: f64 = 72.0;
const ACCENT_COLOR: (f64, f64, f64, f64) = (0.31, 0.82, 0.7, 0.94);
const SIGNAL_COLOR: (f64, f64, f64, f64) = (0.95, 0.55, 0.23, 0.96);
const IVORY_COLOR: (f64, f64, f64, f64) = (0.91, 0.89, 0.79, 1.0);
const SECONDARY_COLOR: (f64, f64, f64, f64) = (0.56, 0.61, 0.54, 1.0);
const GLOW_COLOR: (f64, f64, f64) = (0.24, 0.68, 0.58);
const GLOW_CENTER_ALPHA: f64 = 0.22;
const GLOW_MIDDLE_ALPHA: f64 = 0.075;
const GLOW_MIN_OPACITY: f32 = 0.6;
const GLOW_MAX_OPACITY: f32 = 0.86;
const GLOW_STATIC_OPACITY: f32 = 0.72;
const GLOW_HALF_CYCLE_SECONDS: f64 = 4.2;
const GLOW_ANIMATION_KEY: &str = "rift.command-palette.ambient-glow";
const GLOW_HORIZONTAL_POSITION: f64 = 0.618_033_988_75;
const PANEL_TINT_ALPHA: f64 = 0.72;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteInput {
    QueryChanged(String),
    MoveSelection(isize),
    Execute,
    ExpandApplication,
    LeaveApplication,
    Cancel,
    ClickResult(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalettePanelMode {
    Windows,
    Commands,
}

impl PalettePanelMode {
    fn brand_label(self) -> &'static str {
        match self {
            Self::Windows => "RIFT // DECK 01",
            Self::Commands => "RIFT // CMD DECK",
        }
    }

    fn brand_width(self) -> f64 {
        match self {
            Self::Windows => 104.0,
            Self::Commands => 112.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRenderRow {
    pub primary: String,
    pub secondary: String,
    pub kind: PaletteEntryKind,
    pub app_pid: Option<i32>,
}

type InputHandler = Rc<dyn Fn(PaletteInput)>;

struct PaletteSearchFieldIvars {
    callback: InputHandler,
}

define_class!(
    #[unsafe(super(NSTextField))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteSearchField"]
    #[ivars = PaletteSearchFieldIvars]
    struct PaletteSearchField;

    impl PaletteSearchField {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let input = match event.keyCode() {
                53 => Some(PaletteInput::Cancel),
                126 => Some(PaletteInput::MoveSelection(-1)),
                125 => Some(PaletteInput::MoveSelection(1)),
                36 | 76 => Some(PaletteInput::Execute),
                124 => Some(PaletteInput::ExpandApplication),
                123 => Some(PaletteInput::LeaveApplication),
                _ => None,
            };
            if let Some(input) = input {
                (self.ivars().callback)(input);
            } else {
                unsafe {
                    let _: () = msg_send![super(self), keyDown: event];
                }
            }
        }
    }
);

impl PaletteSearchField {
    fn new(mtm: MainThreadMarker, frame: CGRect, callback: InputHandler) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(PaletteSearchFieldIvars { callback });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

define_class!(
    #[unsafe(super(NSScrollView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteScrollView"]
    struct PaletteScrollView;

    impl PaletteScrollView {
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            unsafe {
                let _: () = msg_send![super(self), scrollWheel: event];
            }
            if let Some(document) = self.documentView() {
                document.setNeedsDisplay(true);
            }
        }
    }
);

impl PaletteScrollView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        unsafe { msg_send![mtm.alloc::<Self>(), initWithFrame: frame] }
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteAmbientGlowView"]
    struct PaletteAmbientGlowView;

    impl PaletteAmbientGlowView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: CGRect) {
            draw_ambient_glow(self.bounds());
            draw_cassette_chrome(self.bounds());
        }
    }
);

impl PaletteAmbientGlowView {
    fn new(mtm: MainThreadMarker, frame: CGRect) -> Retained<Self> {
        unsafe { msg_send![mtm.alloc::<Self>(), initWithFrame: frame] }
    }
}

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftCommandPalettePanel"]
    struct PalettePanel;

    impl PalettePanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool { true }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool { false }
    }
);

struct PaletteTextHandlerIvars {
    callback: InputHandler,
    text_field: Retained<PaletteSearchField>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteTextHandler"]
    #[ivars = PaletteTextHandlerIvars]
    struct PaletteTextHandler;

    unsafe impl NSObjectProtocol for PaletteTextHandler {}

    unsafe impl NSControlTextEditingDelegate for PaletteTextHandler {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            let query = self.ivars().text_field.stringValue().to_string();
            (self.ivars().callback)(PaletteInput::QueryChanged(query));
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        unsafe fn control_text_view_do_command(
            &self,
            _control: &NSControl,
            _text_view: &NSTextView,
            command: objc2::runtime::Sel,
        ) -> bool {
            let input = if command == sel!(moveUp:) {
                Some(PaletteInput::MoveSelection(-1))
            } else if command == sel!(moveDown:) {
                Some(PaletteInput::MoveSelection(1))
            } else if command == sel!(insertNewline:)
                || command == sel!(insertNewlineIgnoringFieldEditor:)
            {
                Some(PaletteInput::Execute)
            } else if command == sel!(cancelOperation:) {
                Some(PaletteInput::Cancel)
            } else if command == sel!(moveRight:) {
                Some(PaletteInput::ExpandApplication)
            } else if command == sel!(moveLeft:) {
                Some(PaletteInput::LeaveApplication)
            } else {
                None
            };
            if let Some(input) = input {
                (self.ivars().callback)(input);
                true
            } else {
                false
            }
        }
    }

    unsafe impl NSTextFieldDelegate for PaletteTextHandler {}
);

impl PaletteTextHandler {
    fn new(
        mtm: MainThreadMarker,
        text_field: Retained<PaletteSearchField>,
        callback: InputHandler,
    ) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(PaletteTextHandlerIvars { callback, text_field });
        unsafe { msg_send![super(this), init] }
    }
}

struct PaletteWindowHandlerIvars {
    callback: InputHandler,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteWindowHandler"]
    #[ivars = PaletteWindowHandlerIvars]
    struct PaletteWindowHandler;

    unsafe impl NSObjectProtocol for PaletteWindowHandler {}

    unsafe impl NSWindowDelegate for PaletteWindowHandler {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            (self.ivars().callback)(PaletteInput::Cancel);
        }
    }
);

impl PaletteWindowHandler {
    fn new(mtm: MainThreadMarker, callback: InputHandler) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(PaletteWindowHandlerIvars { callback });
        unsafe { msg_send![super(this), init] }
    }
}

#[derive(Default)]
struct ResultsState {
    rows: Vec<PaletteRenderRow>,
    selected: Option<usize>,
}

struct PaletteResultsViewIvars {
    state: RefCell<ResultsState>,
    callback: InputHandler,
    viewport_size: Cell<CGSize>,
    primary_attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
    secondary_attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
    empty_attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>>,
    text_metrics: RowTextMetrics,
    icons: RefCell<HashMap<i32, Retained<NSImage>>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiftPaletteResultsView"]
    #[ivars = PaletteResultsViewIvars]
    struct PaletteResultsView;

    impl PaletteResultsView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty_rect: CGRect) {
            self.draw_results(dirty_rect);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if point.y < 0.0 {
                return;
            }
            let index = (point.y / ROW_HEIGHT).floor() as usize;
            if index < self.ivars().state.borrow().rows.len() {
                (self.ivars().callback)(PaletteInput::ClickResult(index));
            }
        }
    }
);

impl PaletteResultsView {
    fn new(mtm: MainThreadMarker, frame: CGRect, callback: InputHandler) -> Retained<Self> {
        let primary_font = NSFont::systemFontOfSize_weight(14.5, unsafe { NSFontWeightMedium });
        let secondary_font =
            NSFont::monospacedSystemFontOfSize_weight(10.5, unsafe { NSFontWeightMedium });
        let empty_font =
            NSFont::monospacedSystemFontOfSize_weight(11.0, unsafe { NSFontWeightMedium });
        let primary_attributes =
            text_attributes(primary_font.as_ref(), palette_color(IVORY_COLOR).as_ref());
        let secondary_attributes =
            text_attributes(secondary_font.as_ref(), palette_color(SECONDARY_COLOR).as_ref());
        let empty_attributes =
            text_attributes(empty_font.as_ref(), palette_color(SECONDARY_COLOR).as_ref());
        let text_metrics = RowTextMetrics {
            primary_line_height: font_line_height(primary_font.as_ref()),
            secondary_line_height: font_line_height(secondary_font.as_ref()),
            empty_line_height: font_line_height(empty_font.as_ref()),
        };
        let this = mtm.alloc().set_ivars(PaletteResultsViewIvars {
            state: RefCell::new(ResultsState::default()),
            callback,
            viewport_size: Cell::new(frame.size),
            primary_attributes,
            secondary_attributes,
            empty_attributes,
            text_metrics,
            icons: RefCell::new(HashMap::new()),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_viewport_size(&self, viewport_size: CGSize) {
        self.ivars().viewport_size.set(viewport_size);
        let row_count = self.ivars().state.borrow().rows.len();
        self.setFrameSize(CGSize::new(
            viewport_size.width,
            result_document_height(row_count, viewport_size.height),
        ));
    }

    fn set_rows(&self, rows: Vec<PaletteRenderRow>, selected: Option<usize>) {
        let viewport_size = self.ivars().viewport_size.get();
        let document_height = result_document_height(rows.len(), viewport_size.height);
        self.setFrameSize(CGSize::new(viewport_size.width, document_height));
        *self.ivars().state.borrow_mut() = ResultsState { rows, selected };
        if let Some(index) = selected {
            let _ = self.scrollRectToVisible(CGRect::new(
                CGPoint::new(0.0, index as f64 * ROW_HEIGHT),
                CGSize::new(viewport_size.width, ROW_HEIGHT),
            ));
        }
        self.setNeedsDisplay(true);
    }

    fn draw_results(&self, dirty_rect: CGRect) {
        let Some(graphics) = NSGraphicsContext::currentContext() else {
            return;
        };
        let context = graphics.CGContext();
        let state = self.ivars().state.borrow();
        if state.rows.is_empty() {
            let line_height = self.ivars().text_metrics.empty_line_height;
            unsafe {
                NSString::from_str("No matching windows, apps, or commands")
                    .drawInRect_withAttributes(
                        CGRect::new(
                            CGPoint::new(50.0, centered_origin(0.0, ROW_HEIGHT, line_height)),
                            CGSize::new(self.bounds().size.width - 64.0, line_height),
                        ),
                        Some(self.ivars().empty_attributes.as_ref()),
                    );
            }
            return;
        }
        for index in visible_row_range(dirty_rect, state.rows.len()) {
            let row = &state.rows[index];
            let y = index as f64 * ROW_HEIGHT;
            fill_rounded_rect(
                context.as_ref(),
                CGRect::new(
                    CGPoint::new(9.0, y + ROW_HEIGHT - 0.5),
                    CGSize::new(self.bounds().size.width - 22.0, 0.5),
                ),
                0.0,
                (0.5, 0.56, 0.49, 0.14),
            );
            if state.selected == Some(index) {
                fill_rounded_rect(
                    context.as_ref(),
                    CGRect::new(
                        CGPoint::new(2.0, y + 2.0),
                        CGSize::new(self.bounds().size.width - 8.0, ROW_HEIGHT - 4.0),
                    ),
                    3.0,
                    (0.12, 0.23, 0.2, 0.72),
                );
                stroke_rounded_rect(
                    context.as_ref(),
                    CGRect::new(
                        CGPoint::new(2.5, y + 2.5),
                        CGSize::new(self.bounds().size.width - 9.0, ROW_HEIGHT - 5.0),
                    ),
                    3.0,
                    (ACCENT_COLOR.0, ACCENT_COLOR.1, ACCENT_COLOR.2, 0.26),
                    0.75,
                );
                fill_rounded_rect(
                    context.as_ref(),
                    CGRect::new(CGPoint::new(3.0, y + 8.0), CGSize::new(3.0, ROW_HEIGHT - 16.0)),
                    1.5,
                    SIGNAL_COLOR,
                );
            }

            let icon_bezel = CGRect::new(
                CGPoint::new(10.5, y + (ROW_HEIGHT - 31.0) / 2.0),
                CGSize::new(31.0, 31.0),
            );
            fill_rounded_rect(context.as_ref(), icon_bezel, 5.0, (0.025, 0.035, 0.03, 0.72));
            stroke_rounded_rect(context.as_ref(), icon_bezel, 5.0, (0.55, 0.6, 0.52, 0.22), 0.75);
            let icon_rect = CGRect::new(
                CGPoint::new(13.0, y + (ROW_HEIGHT - ICON_SIZE) / 2.0),
                CGSize::new(ICON_SIZE, ICON_SIZE),
            );
            if let Some(icon) = row.app_pid.and_then(|pid| self.icon_for_pid(pid)) {
                icon.drawInRect(icon_rect);
            } else {
                let glyph = match row.kind {
                    PaletteEntryKind::Window => "WIN",
                    PaletteEntryKind::Application => "APP",
                    PaletteEntryKind::Command => "CMD",
                };
                let glyph_rect = CGRect::new(
                    CGPoint::new(
                        icon_rect.origin.x + 2.0,
                        centered_origin(
                            icon_rect.origin.y,
                            icon_rect.size.height,
                            self.ivars().text_metrics.secondary_line_height,
                        ),
                    ),
                    CGSize::new(
                        icon_rect.size.width - 4.0,
                        self.ivars().text_metrics.secondary_line_height,
                    ),
                );
                unsafe {
                    NSString::from_str(glyph).drawInRect_withAttributes(
                        glyph_rect,
                        Some(self.ivars().secondary_attributes.as_ref()),
                    );
                }
            }

            let text_x = 51.0;
            let metadata = NSString::from_str(&row.secondary);
            let metadata_width = unsafe {
                metadata.sizeWithAttributes(Some(self.ivars().secondary_attributes.as_ref()))
            }
            .width;
            let text_layout = row_text_layout(
                self.bounds().size.width,
                text_x,
                metadata_width,
                y,
                self.ivars().text_metrics,
            );
            unsafe {
                NSString::from_str(&row.primary).drawWithRect_options_attributes_context(
                    text_layout.title,
                    NSStringDrawingOptions::UsesLineFragmentOrigin
                        | NSStringDrawingOptions::TruncatesLastVisibleLine,
                    Some(self.ivars().primary_attributes.as_ref()),
                    None,
                );
                metadata.drawWithRect_options_attributes_context(
                    text_layout.metadata,
                    NSStringDrawingOptions::UsesLineFragmentOrigin
                        | NSStringDrawingOptions::TruncatesLastVisibleLine,
                    Some(self.ivars().secondary_attributes.as_ref()),
                    None,
                );
            }
        }

        if let Some(indicator) =
            scroll_indicator_frame(self.bounds().size.height, self.visibleRect())
        {
            fill_rounded_rect(
                context.as_ref(),
                indicator,
                indicator.size.width / 2.0,
                (SIGNAL_COLOR.0, SIGNAL_COLOR.1, SIGNAL_COLOR.2, 0.48),
            );
        }
    }

    fn icon_for_pid(&self, pid: i32) -> Option<Retained<NSImage>> {
        if let Some(icon) = self.ivars().icons.borrow().get(&pid) {
            return Some(icon.clone());
        }
        let icon = NSRunningApplication::with_process_id(pid)?.icon()?;
        self.ivars().icons.borrow_mut().insert(pid, icon.clone());
        Some(icon)
    }
}

pub struct CommandPalettePanel {
    panel: Retained<PalettePanel>,
    content: Retained<NSView>,
    backdrop: Retained<NSVisualEffectView>,
    surface: Retained<NSView>,
    ambient_glow: Retained<PaletteAmbientGlowView>,
    brand_label: Retained<NSTextField>,
    search_field: Retained<PaletteSearchField>,
    scroll_view: Retained<PaletteScrollView>,
    results_view: Retained<PaletteResultsView>,
    result_count: Retained<NSTextField>,
    header_metrics: HeaderMetrics,
    _text_handler: Retained<PaletteTextHandler>,
    _window_handler: Retained<PaletteWindowHandler>,
    width: f64,
    max_visible_rows: usize,
    height: Cell<f64>,
    mode: Cell<PalettePanelMode>,
    mtm: MainThreadMarker,
}

impl CommandPalettePanel {
    pub fn new(
        mtm: MainThreadMarker,
        width: f64,
        visible_rows: usize,
        callback: InputHandler,
    ) -> Self {
        let max_visible_rows = visible_rows.max(1);
        let mode = PalettePanelMode::Windows;
        let height = panel_height(max_visible_rows);
        let results_frame = results_viewport_frame(width, max_visible_rows);
        let header_y = header_band_y(max_visible_rows);
        let frame = CGRect::new(CGPoint::ZERO, CGSize::new(width, height));
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<PalettePanel> = unsafe {
            msg_send![
                mtm.alloc::<PalettePanel>(),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(NSColor::clearColor().as_ref()));
        panel.setHasShadow(true);
        panel.setLevel(NSPopUpMenuWindowLevel);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::Transient
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let content = NSView::initWithFrame(mtm.alloc(), frame);
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            layer.setCornerRadius(12.0);
            layer.setMasksToBounds(true);
            layer.setBorderWidth(1.0);
            let border =
                NSColor::colorWithSRGBRed_green_blue_alpha(0.44, 0.54, 0.47, 0.48).CGColor();
            layer.setBorderColor(Some(border.as_ref()));
        }

        let backdrop = NSVisualEffectView::initWithFrame(mtm.alloc(), frame);
        backdrop.setMaterial(NSVisualEffectMaterial::HUDWindow);
        backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        backdrop.setState(NSVisualEffectState::Active);
        backdrop.setEmphasized(false);

        let surface = NSView::initWithFrame(mtm.alloc(), frame);
        surface.setWantsLayer(true);
        if let Some(layer) = surface.layer() {
            layer.setBackgroundColor(Some(
                NSColor::colorWithSRGBRed_green_blue_alpha(0.045, 0.06, 0.052, PANEL_TINT_ALPHA)
                    .CGColor()
                    .as_ref(),
            ));
        }

        let ambient_glow = PaletteAmbientGlowView::new(mtm, frame);
        ambient_glow.setWantsLayer(true);
        if let Some(layer) = ambient_glow.layer() {
            layer.setOpacity(GLOW_STATIC_OPACITY);
        }

        let brand_label =
            NSTextField::labelWithString(&NSString::from_str(mode.brand_label()), mtm);
        brand_label.setFont(Some(
            NSFont::monospacedSystemFontOfSize_weight(9.5, unsafe { NSFontWeightMedium }).as_ref(),
        ));
        brand_label.setTextColor(Some(palette_color(ACCENT_COLOR).as_ref()));

        let search_frame = header_search_frame(width, header_y, mode, SEARCH_FIELD_HEIGHT);
        let search_field = PaletteSearchField::new(mtm, search_frame, callback.clone());
        search_field
            .setPlaceholderString(Some(&NSString::from_str("QUERY WINDOW / APP / COMMAND")));
        search_field.setBordered(false);
        search_field.setBezeled(false);
        search_field.setDrawsBackground(false);
        search_field.setFocusRingType(NSFocusRingType::None);
        search_field.setEditable(true);
        search_field.setSelectable(true);
        search_field.setUsesSingleLineMode(true);
        search_field.setFont(Some(
            NSFont::monospacedSystemFontOfSize_weight(15.5, unsafe { NSFontWeightMedium }).as_ref(),
        ));
        search_field.setTextColor(Some(palette_color(IVORY_COLOR).as_ref()));

        let results_view = PaletteResultsView::new(
            mtm,
            CGRect::new(CGPoint::ZERO, results_frame.size),
            callback.clone(),
        );
        let scroll_view = PaletteScrollView::new(mtm, results_frame);
        scroll_view.setDrawsBackground(false);
        scroll_view.setBorderType(NSBorderType::NoBorder);
        scroll_view.setHasHorizontalScroller(false);
        scroll_view.setHasVerticalScroller(false);
        scroll_view.setDocumentView(Some(&results_view));

        let result_count = NSTextField::labelWithString(&NSString::from_str("00"), mtm);
        result_count.setFont(Some(
            NSFont::monospacedDigitSystemFontOfSize_weight(10.5, unsafe { NSFontWeightMedium })
                .as_ref(),
        ));
        result_count.setTextColor(Some(palette_color(SIGNAL_COLOR).as_ref()));

        // NSTextFieldCell adds control-specific padding around its font metrics. Ask AppKit for
        // each control's intrinsic single-line height, then center those actual control boxes in
        // the shared header band. This keeps the label, editor/caret, and count geometrically
        // aligned without font- or OS-specific offsets.
        let header_metrics = HeaderMetrics {
            brand_height: brand_label
                .sizeThatFits(CGSize::new(mode.brand_width(), SEARCH_FIELD_HEIGHT))
                .height,
            search_height: search_field
                .sizeThatFits(CGSize::new(search_frame.size.width, SEARCH_FIELD_HEIGHT))
                .height,
            count_height: result_count.sizeThatFits(CGSize::new(38.0, SEARCH_FIELD_HEIGHT)).height,
        };
        let header_layout = header_layout(width, header_y, mode, header_metrics);
        brand_label.setFrame(header_layout.brand);
        search_field.setFrame(header_layout.search);
        result_count.setFrame(header_layout.count);

        content.addSubview(&backdrop);
        content.addSubview(&surface);
        content.addSubview(&ambient_glow);
        content.addSubview(&brand_label);
        content.addSubview(&search_field);
        content.addSubview(&scroll_view);
        content.addSubview(&result_count);
        panel.setContentView(Some(&content));

        let text_handler = PaletteTextHandler::new(mtm, search_field.clone(), callback.clone());
        let text_delegate: &ProtocolObject<dyn NSTextFieldDelegate> =
            ProtocolObject::from_ref::<PaletteTextHandler>(&text_handler);
        unsafe { search_field.setDelegate(Some(text_delegate)) };
        let window_handler = PaletteWindowHandler::new(mtm, callback);
        let window_delegate: &ProtocolObject<dyn NSWindowDelegate> =
            ProtocolObject::from_ref::<PaletteWindowHandler>(&window_handler);
        panel.setDelegate(Some(window_delegate));

        Self {
            panel,
            content,
            backdrop,
            surface,
            ambient_glow,
            brand_label,
            search_field,
            scroll_view,
            results_view,
            result_count,
            header_metrics,
            _text_handler: text_handler,
            _window_handler: window_handler,
            width,
            max_visible_rows,
            height: Cell::new(height),
            mode: Cell::new(mode),
            mtm,
        }
    }

    pub fn show(&self, display_id: Option<u32>, query: &str, mode: PalettePanelMode) -> bool {
        self.set_mode(mode);
        self.set_query(query);
        self.start_ambient_glow();
        let screen = display_id
            .and_then(|display_id| {
                NSScreen::screens(self.mtm).into_iter().find(|screen| {
                    screen.get_number().ok().is_some_and(|id| id.as_u32() == display_id)
                })
            })
            .or_else(|| NSScreen::mainScreen(self.mtm));
        if let Some(screen) = screen {
            let visible = screen.visibleFrame();
            let height = self.height.get();
            let origin = CGPoint::new(
                visible.origin.x + (visible.size.width - self.width) / 2.0,
                visible.origin.y + (visible.size.height - height) * 0.62,
            );
            self.panel
                .setFrame_display(CGRect::new(origin, CGSize::new(self.width, height)), false);
        }
        self.panel.makeKeyAndOrderFront(None);
        let text_field: &NSTextField = self.search_field.as_ref();
        let responder: &objc2_app_kit::NSResponder = text_field.as_ref();
        let responder_ready = self.panel.makeFirstResponder(Some(responder));
        if let Some(editor) = self.search_field.currentEditor() {
            let editor: Retained<NSTextView> = unsafe { Retained::cast_unchecked(editor) };
            editor.setInsertionPointColor(Some(palette_color(ACCENT_COLOR).as_ref()));
        }
        if let Ok(window_number) = u32::try_from(self.panel.windowNumber()) {
            let _ = window_server::make_key_window(
                std::process::id() as i32,
                WindowServerId::new(window_number),
            );
        }
        unsafe { self.search_field.selectText(None) };
        self.panel.isKeyWindow() && responder_ready
    }

    pub fn hide(&self) {
        self.stop_ambient_glow();
        self.panel.orderOut(None);
    }

    pub fn set_query(&self, query: &str) {
        self.search_field.setStringValue(&NSString::from_str(query));
    }

    pub fn render(&self, rows: Vec<PaletteRenderRow>, selected: Option<usize>) {
        self.layout_for_result_count(rows.len());
        self.result_count
            .setStringValue(&NSString::from_str(&format!("CH {:02}", rows.len())));
        self.results_view.set_rows(rows, selected);
    }

    fn layout_for_result_count(&self, result_count: usize) {
        let visible_rows = result_count.clamp(1, self.max_visible_rows);
        let height = panel_height(visible_rows);
        let old_height = self.height.replace(height);
        if (old_height - height).abs() < f64::EPSILON {
            return;
        }

        let current_frame = self.panel.frame();
        let origin = CGPoint::new(
            current_frame.origin.x,
            current_frame.origin.y + current_frame.size.height - height,
        );
        let frame = CGRect::new(origin, CGSize::new(self.width, height));
        self.panel.setFrame_display(frame, false);
        self.content.setFrame(CGRect::new(CGPoint::ZERO, frame.size));
        self.backdrop.setFrame(CGRect::new(CGPoint::ZERO, frame.size));
        self.surface.setFrame(CGRect::new(CGPoint::ZERO, frame.size));
        self.ambient_glow.setFrame(CGRect::new(CGPoint::ZERO, frame.size));
        self.ambient_glow.setNeedsDisplay(true);

        let header_y = header_band_y(visible_rows);
        let mode = self.mode.get();
        let header_layout = header_layout(self.width, header_y, mode, self.header_metrics);
        self.brand_label.setFrame(header_layout.brand);
        self.search_field.setFrame(header_layout.search);
        self.result_count.setFrame(header_layout.count);

        let results_frame = results_viewport_frame(self.width, visible_rows);
        self.scroll_view.setFrame(results_frame);
        self.results_view.set_viewport_size(results_frame.size);
    }

    fn set_mode(&self, mode: PalettePanelMode) {
        self.mode.set(mode);
        self.brand_label.setStringValue(&NSString::from_str(mode.brand_label()));
        let visible_rows = ((self.scroll_view.frame().size.height / ROW_HEIGHT).round() as usize)
            .clamp(1, self.max_visible_rows);
        let header_layout = header_layout(
            self.width,
            header_band_y(visible_rows),
            mode,
            self.header_metrics,
        );
        self.brand_label.setFrame(header_layout.brand);
        self.search_field.setFrame(header_layout.search);
        self.result_count.setFrame(header_layout.count);
    }

    fn start_ambient_glow(&self) {
        let Some(layer) = self.ambient_glow.layer() else {
            return;
        };
        layer.removeAnimationForKey(&NSString::from_str(GLOW_ANIMATION_KEY));
        if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
            layer.setOpacity(GLOW_STATIC_OPACITY);
            return;
        }

        let animation =
            CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
        let from = NSNumber::numberWithFloat(GLOW_MIN_OPACITY);
        let to = NSNumber::numberWithFloat(GLOW_MAX_OPACITY);
        let from: &AnyObject = from.as_ref();
        let to: &AnyObject = to.as_ref();
        unsafe {
            animation.setFromValue(Some(from));
            animation.setToValue(Some(to));
        }
        animation.setDuration(GLOW_HALF_CYCLE_SECONDS);
        animation.setAutoreverses(true);
        animation.setRepeatCount(f32::INFINITY);
        animation.setTimingFunction(Some(
            CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionEaseInEaseOut })
                .as_ref(),
        ));
        layer.setOpacity(GLOW_STATIC_OPACITY);
        layer.addAnimation_forKey(&animation, Some(&NSString::from_str(GLOW_ANIMATION_KEY)));
    }

    fn stop_ambient_glow(&self) {
        if let Some(layer) = self.ambient_glow.layer() {
            layer.removeAnimationForKey(&NSString::from_str(GLOW_ANIMATION_KEY));
            layer.setOpacity(GLOW_STATIC_OPACITY);
        }
    }
}

fn panel_height(visible_rows: usize) -> f64 {
    RESULTS_BOTTOM_PADDING
        + visible_rows.max(1) as f64 * ROW_HEIGHT
        + SEARCH_RESULTS_GAP
        + SEARCH_FIELD_HEIGHT
        + TOP_PADDING
}

fn header_band_y(visible_rows: usize) -> f64 {
    RESULTS_BOTTOM_PADDING + visible_rows.max(1) as f64 * ROW_HEIGHT + SEARCH_RESULTS_GAP
}

fn header_band_frame(bounds: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(
            bounds.origin.x,
            bounds.origin.y + bounds.size.height - TOP_PADDING - SEARCH_FIELD_HEIGHT,
        ),
        CGSize::new(bounds.size.width, SEARCH_FIELD_HEIGHT),
    )
}

fn header_chrome_frame(bounds: CGRect) -> CGRect {
    let band = header_band_frame(bounds);
    CGRect::new(
        CGPoint::new(
            bounds.origin.x + HEADER_CHROME_HORIZONTAL_INSET,
            band.origin.y - HEADER_CHROME_VERTICAL_PADDING,
        ),
        CGSize::new(
            bounds.size.width - HEADER_CHROME_HORIZONTAL_INSET * 2.0,
            band.size.height + HEADER_CHROME_VERTICAL_PADDING * 2.0,
        ),
    )
}

fn results_viewport_frame(width: f64, visible_rows: usize) -> CGRect {
    CGRect::new(
        CGPoint::new(PANEL_PADDING, RESULTS_BOTTOM_PADDING),
        CGSize::new(
            width - PANEL_PADDING * 2.0,
            visible_rows.max(1) as f64 * ROW_HEIGHT,
        ),
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeaderMetrics {
    brand_height: f64,
    search_height: f64,
    count_height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeaderLayout {
    brand: CGRect,
    search: CGRect,
    count: CGRect,
}

fn header_layout(
    width: f64,
    header_y: f64,
    mode: PalettePanelMode,
    metrics: HeaderMetrics,
) -> HeaderLayout {
    HeaderLayout {
        brand: CGRect::new(
            CGPoint::new(
                PANEL_PADDING + 3.0,
                centered_origin(header_y, SEARCH_FIELD_HEIGHT, metrics.brand_height),
            ),
            CGSize::new(mode.brand_width(), metrics.brand_height),
        ),
        search: header_search_frame(width, header_y, mode, metrics.search_height),
        count: CGRect::new(
            CGPoint::new(
                width - 51.0,
                centered_origin(header_y, SEARCH_FIELD_HEIGHT, metrics.count_height),
            ),
            CGSize::new(38.0, metrics.count_height),
        ),
    }
}

fn header_search_frame(width: f64, header_y: f64, mode: PalettePanelMode, height: f64) -> CGRect {
    let search_x = PANEL_PADDING + mode.brand_width() + 10.0;
    CGRect::new(
        CGPoint::new(search_x, centered_origin(header_y, SEARCH_FIELD_HEIGHT, height)),
        CGSize::new((width - search_x - 58.0).max(0.0), height),
    )
}

fn centered_origin(container_origin: f64, container_size: f64, content_size: f64) -> f64 {
    container_origin + (container_size - content_size) / 2.0
}

fn font_line_height(font: &NSFont) -> f64 {
    (font.ascender() - font.descender() + font.leading()).ceil()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RowTextLayout {
    title: CGRect,
    metadata: CGRect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RowTextMetrics {
    primary_line_height: f64,
    secondary_line_height: f64,
    empty_line_height: f64,
}

fn row_text_layout(
    view_width: f64,
    text_x: f64,
    measured_metadata_width: f64,
    row_y: f64,
    metrics: RowTextMetrics,
) -> RowTextLayout {
    let right_padding = 15.0;
    let column_gap = 18.0;
    let available_width = (view_width - text_x - right_padding).max(0.0);
    let metadata_width = measured_metadata_width
        .ceil()
        .min((available_width - MIN_TITLE_WIDTH - column_gap).max(0.0));
    let metadata_x = view_width - right_padding - metadata_width;
    let title_width = (metadata_x - column_gap - text_x).max(0.0);

    RowTextLayout {
        title: CGRect::new(
            CGPoint::new(
                text_x,
                centered_origin(row_y, ROW_HEIGHT, metrics.primary_line_height),
            ),
            CGSize::new(title_width, metrics.primary_line_height),
        ),
        metadata: CGRect::new(
            CGPoint::new(
                metadata_x,
                centered_origin(row_y, ROW_HEIGHT, metrics.secondary_line_height),
            ),
            CGSize::new(metadata_width, metrics.secondary_line_height),
        ),
    }
}

fn scroll_indicator_frame(document_height: f64, visible_rect: CGRect) -> Option<CGRect> {
    let viewport_height = visible_rect.size.height;
    let max_offset = document_height - viewport_height;
    if max_offset <= 0.5 || viewport_height <= 0.0 {
        return None;
    }

    let track_inset = 8.0;
    let track_height = (viewport_height - track_inset * 2.0).max(0.0);
    let thumb_height =
        (viewport_height / document_height * track_height).max(22.0).min(track_height);
    let progress = (visible_rect.origin.y / max_offset).clamp(0.0, 1.0);
    let thumb_y =
        visible_rect.origin.y + track_inset + progress * (track_height - thumb_height).max(0.0);
    Some(CGRect::new(
        CGPoint::new(visible_rect.origin.x + visible_rect.size.width - 3.0, thumb_y),
        CGSize::new(2.0, thumb_height),
    ))
}

fn result_document_height(row_count: usize, viewport_height: f64) -> f64 {
    (row_count as f64 * ROW_HEIGHT).max(viewport_height)
}

fn visible_row_range(dirty_rect: CGRect, row_count: usize) -> std::ops::Range<usize> {
    let start = (dirty_rect.origin.y.max(0.0) / ROW_HEIGHT).floor() as usize;
    let end =
        ((dirty_rect.origin.y + dirty_rect.size.height).max(0.0) / ROW_HEIGHT).ceil() as usize;
    start.min(row_count)..end.min(row_count)
}

fn draw_ambient_glow(bounds: CGRect) {
    let Some(graphics) = NSGraphicsContext::currentContext() else {
        return;
    };
    let Some(color_space) = CGColorSpace::new_device_rgb() else {
        return;
    };
    let components = [
        GLOW_COLOR.0,
        GLOW_COLOR.1,
        GLOW_COLOR.2,
        GLOW_CENTER_ALPHA,
        GLOW_COLOR.0,
        GLOW_COLOR.1,
        GLOW_COLOR.2,
        GLOW_MIDDLE_ALPHA,
        GLOW_COLOR.0,
        GLOW_COLOR.1,
        GLOW_COLOR.2,
        0.0,
    ];
    let locations = [0.0, 0.42, 1.0];
    // SAFETY: Core Graphics copies three RGBA component groups and their three
    // locations during this call; both fixed-size arrays remain valid throughout it.
    let Some(gradient) = (unsafe {
        CGGradient::with_color_components(
            Some(color_space.as_ref()),
            components.as_ptr(),
            locations.as_ptr(),
            locations.len(),
        )
    }) else {
        return;
    };

    let (source, falloff) = ambient_glow_geometry();
    let start = CGPoint::new(
        bounds.origin.x + bounds.size.width * source.x,
        bounds.origin.y + bounds.size.height * source.y,
    );
    let end = CGPoint::new(
        bounds.origin.x + bounds.size.width * falloff.x,
        bounds.origin.y + bounds.size.height * falloff.y,
    );
    CGContext::draw_radial_gradient(
        Some(graphics.CGContext().as_ref()),
        Some(gradient.as_ref()),
        start,
        0.0,
        end,
        bounds.size.width * 0.55,
        CGGradientDrawingOptions::empty(),
    );
}

fn ambient_glow_geometry() -> (CGPoint, CGPoint) {
    (
        CGPoint::new(GLOW_HORIZONTAL_POSITION, 0.96),
        CGPoint::new(GLOW_HORIZONTAL_POSITION, 0.34),
    )
}

fn draw_cassette_chrome(bounds: CGRect) {
    let Some(graphics) = NSGraphicsContext::currentContext() else {
        return;
    };
    let context = graphics.CGContext();
    let context = context.as_ref();
    let header = header_chrome_frame(bounds);
    fill_rounded_rect(context, header, 4.0, (0.018, 0.03, 0.026, 0.68));
    stroke_rounded_rect(context, header, 4.0, (0.46, 0.57, 0.49, 0.28), 0.75);

    fill_rounded_rect(
        context,
        CGRect::new(
            CGPoint::new(
                bounds.origin.x + 16.0,
                bounds.origin.y + bounds.size.height - 9.0,
            ),
            CGSize::new(bounds.size.width * 0.38, 1.0),
        ),
        0.0,
        (ACCENT_COLOR.0, ACCENT_COLOR.1, ACCENT_COLOR.2, 0.52),
    );
    fill_rounded_rect(
        context,
        CGRect::new(
            CGPoint::new(
                bounds.origin.x + 18.0 + bounds.size.width * 0.38,
                bounds.origin.y + bounds.size.height - 9.0,
            ),
            CGSize::new(38.0, 1.0),
        ),
        0.0,
        (SIGNAL_COLOR.0, SIGNAL_COLOR.1, SIGNAL_COLOR.2, 0.82),
    );

    let slot_y = bounds.origin.y + 5.0;
    for index in 0..3 {
        fill_rounded_rect(
            context,
            CGRect::new(
                CGPoint::new(bounds.origin.x + 20.0 + index as f64 * 10.0, slot_y),
                CGSize::new(6.0, 1.5),
            ),
            0.75,
            (0.54, 0.59, 0.51, 0.26),
        );
    }
}

fn palette_color(color: (f64, f64, f64, f64)) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(color.0, color.1, color.2, color.3)
}

fn text_attributes(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let attributes = NSMutableDictionary::<NSAttributedStringKey, AnyObject>::new();
    unsafe {
        attributes.setObject_forKeyedSubscript(
            Some(as_any_object(font)),
            ProtocolObject::from_ref(NSFontAttributeName),
        );
        attributes.setObject_forKeyedSubscript(
            Some(as_any_object(color)),
            ProtocolObject::from_ref(NSForegroundColorAttributeName),
        );
        Retained::cast_unchecked(attributes)
    }
}

fn as_any_object<T: Message>(object: &T) -> &AnyObject {
    // SAFETY: Objective-C class references share the NSObject-compatible object layout.
    unsafe { &*(object as *const T as *const AnyObject) }
}

fn fill_rounded_rect(context: &CGContext, rect: CGRect, radius: f64, color: (f64, f64, f64, f64)) {
    add_rounded_rect_path(context, rect, radius);
    CGContext::set_rgb_fill_color(Some(context), color.0, color.1, color.2, color.3);
    CGContext::fill_path(Some(context));
}

fn stroke_rounded_rect(
    context: &CGContext,
    rect: CGRect,
    radius: f64,
    color: (f64, f64, f64, f64),
    width: f64,
) {
    add_rounded_rect_path(context, rect, radius);
    CGContext::set_rgb_stroke_color(Some(context), color.0, color.1, color.2, color.3);
    CGContext::set_line_width(Some(context), width);
    CGContext::stroke_path(Some(context));
}

fn add_rounded_rect_path(context: &CGContext, rect: CGRect, radius: f64) {
    let radius = radius.min(rect.size.width / 2.0).min(rect.size.height / 2.0);
    CGContext::begin_path(Some(context));
    CGContext::move_to_point(Some(context), rect.origin.x + radius, rect.origin.y);
    CGContext::add_arc_to_point(
        Some(context),
        rect.origin.x + rect.size.width,
        rect.origin.y,
        rect.origin.x + rect.size.width,
        rect.origin.y + radius,
        radius,
    );
    CGContext::add_arc_to_point(
        Some(context),
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
        rect.origin.x + rect.size.width - radius,
        rect.origin.y + rect.size.height,
        radius,
    );
    CGContext::add_arc_to_point(
        Some(context),
        rect.origin.x,
        rect.origin.y + rect.size.height,
        rect.origin.x,
        rect.origin.y + rect.size.height - radius,
        radius,
    );
    CGContext::add_arc_to_point(
        Some(context),
        rect.origin.x,
        rect.origin.y,
        rect.origin.x + radius,
        rect.origin.y,
        radius,
    );
    CGContext::close_path(Some(context));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_height_grows_beyond_the_viewport_without_truncating_rows() {
        assert_eq!(result_document_height(25, ROW_HEIGHT * 10.0), ROW_HEIGHT * 25.0);
        assert_eq!(result_document_height(2, ROW_HEIGHT * 10.0), ROW_HEIGHT * 10.0);
    }

    #[test]
    fn drawing_only_visits_rows_intersecting_the_dirty_viewport() {
        let dirty = CGRect::new(
            CGPoint::new(0.0, ROW_HEIGHT * 10.0 + 1.0),
            CGSize::new(640.0, ROW_HEIGHT * 3.0),
        );

        assert_eq!(visible_row_range(dirty, 25), 10..14);
    }

    #[test]
    fn compact_panel_height_tracks_visible_rows() {
        assert_eq!(panel_height(8), 410.0);
        assert_eq!(panel_height(3), 190.0);
        assert_eq!(results_viewport_frame(640.0, 8).size, CGSize::new(616.0, 352.0));
    }

    #[test]
    fn header_chrome_uses_the_control_band_and_does_not_overlap_results() {
        let visible_rows = 8;
        let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(640.0, panel_height(visible_rows)));
        let band = header_band_frame(bounds);
        let chrome = header_chrome_frame(bounds);
        let results = results_viewport_frame(bounds.size.width, visible_rows);
        let center = |rect: CGRect| rect.origin.y + rect.size.height / 2.0;

        assert_eq!(band.origin.y, header_band_y(visible_rows));
        assert_eq!(center(chrome), center(band));
        assert_eq!(
            chrome.origin.y - (results.origin.y + results.size.height),
            SEARCH_RESULTS_GAP - HEADER_CHROME_VERTICAL_PADDING
        );
        assert!(chrome.origin.y > results.origin.y + results.size.height);
        assert!(chrome.origin.y + chrome.size.height < bounds.origin.y + bounds.size.height);
    }

    #[test]
    fn custom_scroll_indicator_stays_thin_and_tracks_the_visible_rect() {
        let visible = CGRect::new(CGPoint::new(0.0, 220.0), CGSize::new(616.0, ROW_HEIGHT * 8.0));
        let indicator = scroll_indicator_frame(ROW_HEIGHT * 25.0, visible).unwrap();

        assert_eq!(indicator.size.width, 2.0);
        assert!(indicator.size.height >= 22.0);
        assert!(indicator.origin.y > visible.origin.y);
        assert!(
            indicator.origin.y + indicator.size.height < visible.origin.y + visible.size.height
        );
        assert!(scroll_indicator_frame(visible.size.height, visible).is_none());
    }

    #[test]
    fn row_layout_reserves_complete_metadata_before_truncating_the_title() {
        let metrics = RowTextMetrics {
            primary_line_height: 18.0,
            secondary_line_height: 13.0,
            empty_line_height: 14.0,
        };
        let layout = row_text_layout(616.0, 51.0, 210.4, 0.0, metrics);

        assert_eq!(layout.metadata.size.width, 211.0);
        assert_eq!(layout.metadata.origin.x + layout.metadata.size.width, 601.0);
        assert_eq!(
            layout.title.origin.x + layout.title.size.width + 18.0,
            layout.metadata.origin.x
        );
        assert_eq!(
            layout.title.origin.y + layout.title.size.height / 2.0,
            ROW_HEIGHT / 2.0
        );
        assert_eq!(
            layout.metadata.origin.y + layout.metadata.size.height / 2.0,
            ROW_HEIGHT / 2.0
        );

        let constrained = row_text_layout(300.0, 51.0, 400.0, 0.0, metrics);
        assert_eq!(constrained.title.size.width, MIN_TITLE_WIDTH);
    }

    #[test]
    fn header_controls_share_the_band_center_despite_different_intrinsic_heights() {
        let header_y = 100.0;
        let layout = header_layout(640.0, header_y, PalettePanelMode::Windows, HeaderMetrics {
            brand_height: 12.0,
            search_height: 19.0,
            count_height: 13.0,
        });
        let expected_center = header_y + SEARCH_FIELD_HEIGHT / 2.0;

        for frame in [layout.brand, layout.search, layout.count] {
            assert_eq!(frame.origin.y + frame.size.height / 2.0, expected_center);
        }
    }

    #[test]
    fn commands_header_reserves_space_before_the_search_field() {
        let layout = header_layout(640.0, 100.0, PalettePanelMode::Commands, HeaderMetrics {
            brand_height: 12.0,
            search_height: 19.0,
            count_height: 13.0,
        });

        assert!(
            layout.brand.size.width == PalettePanelMode::Commands.brand_width()
                && layout.search.origin.x >= layout.brand.origin.x + layout.brand.size.width
        );
    }

    #[test]
    fn ambient_glow_light_axis_uses_the_right_golden_ratio() {
        let (source, falloff) = ambient_glow_geometry();

        assert!(
            source.x == GLOW_HORIZONTAL_POSITION
                && (0.9..=1.0).contains(&source.y)
                && falloff.x == source.x
                && falloff.y < 0.5
        );
    }

    #[test]
    fn ambient_glow_breathing_remains_slow_and_controlled() {
        assert!(
            0.5 <= GLOW_MIN_OPACITY
                && GLOW_MIN_OPACITY < GLOW_STATIC_OPACITY
                && GLOW_STATIC_OPACITY < GLOW_MAX_OPACITY
                && GLOW_MAX_OPACITY <= 1.0
                && GLOW_HALF_CYCLE_SECONDS >= 3.0
        );
    }

    #[test]
    fn ambient_glow_effective_peak_stays_below_thirty_percent() {
        assert!(GLOW_CENTER_ALPHA * f64::from(GLOW_MAX_OPACITY) < 0.3);
    }

    #[test]
    fn panel_tint_preserves_backdrop_transparency_without_losing_contrast() {
        assert!((0.6..=0.8).contains(&PANEL_TINT_ALPHA));
    }
}
