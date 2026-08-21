//! Orchestration logic that sits between an MCP tool call and the real
//! native-API traits [`crate::traits`] defines.
//!
//! This is the actual point of the trait split described in
//! `docs/INVARIANTS.md`'s testing harness: `polarize-macos`'s real
//! `ScreenCaptureKit`/`AXUIElement`/`CGEvent`/AppKit calls cannot be
//! exercised in CI, but the logic that decides *what* to call and *with
//! what arguments* can be — and is, against fake trait implementations,
//! below.

use crate::coords::{self, Fraction};
use crate::error::PolarizeError;
use crate::schema::{
    ActivationPath, DescribeRequest, DescribeResponse, KeyboardRequest, KeyboardResponse,
    ScreenshotRequest, ScreenshotResponse, ScreenshotTarget, TapRequest, TapResponse,
};
use crate::traits::{AccessibilityInspector, InputSynthesizer, ScreenCapture, WindowManager};

/// # PINV-4: a tap's fraction is normalized before the platform ever sees it
///
/// - Always: [`perform_tap`] converts `request.x`/`request.y` to a pixel
///   point via [`crate::coords::fraction_to_pixel`] against the resolved
///   target's `size`, adds the target's `origin` to land in the
///   **global** display coordinate space, and only calls
///   [`InputSynthesizer::click_at_pixel`] with that already-resolved
///   global pixel point — never with the raw fraction, and never with a
///   window- or display-relative point.
/// - Because: [`InputSynthesizer`] implementations are real `CGEvent`
///   calls that cannot be exercised in CI; pushing the fraction→pixel
///   decision into this pure, testable function is what lets a fake
///   implementation prove the platform layer receives correct pixel
///   coordinates without ever running on a real screen. `origin` matters
///   because `App`/`Window` targets, and non-primary displays, do not
///   start at the global origin — omitting it clicks whatever happens
///   to sit at that offset on the primary display instead of the
///   intended element.
/// - If violated: a `tap` request appears to succeed but clicks the
///   wrong point — either because the fraction leaked through
///   unconverted, or because it was normalized against the wrong
///   target's size, or because the target's screen origin was dropped.
///
/// See also PINV-47. This function resolves a target pid and passes it
/// to [`InputSynthesizer::click_at_pixel`]. The response's
/// [`crate::schema::PostPath`] always names whichever path the
/// implementation actually ran. It never names the path this function
/// merely requested.
pub fn perform_tap<W, I>(
    window_manager: &W,
    input: &I,
    request: &TapRequest,
) -> Result<TapResponse, PolarizeError>
where
    W: WindowManager,
    I: InputSynthesizer,
{
    let target = request
        .target
        .clone()
        .unwrap_or(ScreenshotTarget::Screen { display_id: None });
    let rect = window_manager.resolve_target_rect(&target)?;
    let local = coords::fraction_to_pixel(
        Fraction {
            x: request.x,
            y: request.y,
        },
        rect.size,
    )?;
    let pixel = crate::coords::PixelPoint {
        x: rect.origin.x + local.x,
        y: rect.origin.y + local.y,
    };
    let click_count = request.click_count.unwrap_or(1);
    // A pid-resolution failure only loses the pid-post optimization.
    // The target itself is already resolved (`resolve_target_rect`
    // above succeeded), so the click still runs on the global fallback
    // path. See PINV-47.
    let pid = window_manager.resolve_target_pid(&target).unwrap_or(None);
    let post_path = input.click_at_pixel(pixel, click_count, pid)?;
    Ok(TapResponse {
        tapped: true,
        pixel_x: pixel.x,
        pixel_y: pixel.y,
        post_path,
    })
}

/// Dispatches a `describe` request to the resolved app (or the
/// frontmost app when `request.app` is `None`) and shapes the result
/// into a [`DescribeResponse`], including its `formatted` rendering
/// (PINV-3).
pub fn perform_describe<A>(
    inspector: &A,
    request: &DescribeRequest,
) -> Result<DescribeResponse, PolarizeError>
where
    A: AccessibilityInspector,
{
    let (resolved, root) = inspector.describe(request.app.as_ref())?;
    let formatted = crate::ax::format_tree(&root);
    Ok(DescribeResponse {
        app_name: resolved.name,
        bundle_id: resolved.bundle_id,
        root,
        formatted,
    })
}

/// Dispatches a `screenshot` request's [`ScreenshotTarget`] to the
/// matching [`ScreenCapture`] call and base64-encodes the result.
pub fn perform_screenshot<C>(
    capture: &C,
    request: &ScreenshotRequest,
) -> Result<ScreenshotResponse, PolarizeError>
where
    C: ScreenCapture,
{
    let image = match &request.target {
        ScreenshotTarget::Screen { display_id } => capture.capture_screen(*display_id)?,
        ScreenshotTarget::App { app } => capture.capture_window(app, None)?,
        ScreenshotTarget::Window { app, window_title } => {
            capture.capture_window(app, Some(window_title.as_str()))?
        }
    };
    Ok(ScreenshotResponse::from_png_bytes(
        &image.png_bytes,
        image.width,
        image.height,
    ))
}

/// # PINV-14: a `keyboard` request activates its target app first
///
/// - Always: when `request` names a `target` app, [`perform_keyboard`]
///   activates it before calling either [`InputSynthesizer::type_text`]
///   or [`InputSynthesizer::press_key`]. When `target` is `None`, it
///   activates nothing.
/// - Because: `CGEvent` posts to whichever app is currently frontmost,
///   not to an app named in the request. Without activating the target
///   first, a `target`-scoped `keyboard` call would silently type into
///   whatever app the user happened to have focused instead.
/// - If violated: text or key presses land in the wrong app, or — if
///   `target` is dropped from the schema entirely instead of wired up —
///   `polarize` advertises a field its `keyboard` tool never honors.
///
/// See also PINV-48: activation itself now has two tiers.
/// [`perform_keyboard`] tries [`WindowManager::activate_app_without_raise`]
/// first. It falls back to [`WindowManager::activate_app`] only when the
/// raise-free path reports itself unavailable. `KeyboardResponse.activation_path`
/// always names whichever tier actually ran.
///
/// See also PINV-49: the key events themselves can now post by pid too,
/// the same way [`perform_tap`] does. `KeyboardResponse.post_path`
/// always names whichever path [`InputSynthesizer::type_text`] or
/// [`InputSynthesizer::press_key`] actually took.
pub fn perform_keyboard<W, I>(
    window_manager: &W,
    input: &I,
    request: &KeyboardRequest,
) -> Result<KeyboardResponse, PolarizeError>
where
    W: WindowManager,
    I: InputSynthesizer,
{
    let target = match request {
        KeyboardRequest::Type { target, .. } => target,
        KeyboardRequest::KeyPress { target, .. } => target,
    };
    let activation_path = match target {
        None => ActivationPath::None,
        Some(app) => {
            if window_manager.activate_app_without_raise(app)? {
                ActivationPath::RaiseFree
            } else {
                window_manager.activate_app(app)?;
                ActivationPath::Raised
            }
        }
    };
    // A pid-resolution failure only loses the pid-post optimization —
    // activation above already succeeded either way. See PINV-49.
    let pid = match target {
        None => None,
        Some(app) => window_manager.resolve_app_pid(app).unwrap_or(None),
    };
    let post_path = match request {
        KeyboardRequest::Type { text, .. } => input.type_text(text, pid)?,
        KeyboardRequest::KeyPress { key, modifiers, .. } => {
            input.press_key(*key, modifiers, pid)?
        }
    };
    Ok(KeyboardResponse {
        sent: true,
        activation_path,
        post_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::{AxNode, NormalizedFrame};
    use crate::coords::{PixelPoint, PixelRect, PixelSize};
    use crate::schema::{AppIdentifier, Modifier, NamedKey, PostPath};
    use crate::traits::CapturedImage;
    use std::cell::RefCell;

    // ---- fakes -------------------------------------------------------

    struct FakeWindowManager {
        rect: PixelRect,
        /// The pid `resolve_target_pid` reports for every target. `None`
        /// matches a `Screen` target, or an app this fake pretends is
        /// not running.
        pid: Option<i32>,
        activated: RefCell<Vec<AppIdentifier>>,
        raise_free_activated: RefCell<Vec<AppIdentifier>>,
        /// What `activate_app_without_raise` reports. `false` by
        /// default, matching the pre-PINV-48 world: a test opts in with
        /// [`Self::with_raise_free_available`] to exercise the new
        /// path.
        raise_free_available: bool,
    }

    impl FakeWindowManager {
        /// A target at the global origin — what every existing test
        /// before PINV-4's origin fix assumed. No pid resolves, as if
        /// every request named the whole screen.
        fn new(size: PixelSize) -> Self {
            Self::with_origin(PixelPoint { x: 0.0, y: 0.0 }, size)
        }

        fn with_origin(origin: PixelPoint, size: PixelSize) -> Self {
            Self {
                rect: PixelRect { origin, size },
                pid: None,
                activated: RefCell::new(Vec::new()),
                raise_free_activated: RefCell::new(Vec::new()),
                raise_free_available: false,
            }
        }

        /// The same target rect, but `resolve_target_pid` reports `pid`
        /// — as if the request named a running app.
        fn with_pid(size: PixelSize, pid: i32) -> Self {
            Self {
                rect: PixelRect {
                    origin: PixelPoint { x: 0.0, y: 0.0 },
                    size,
                },
                pid: Some(pid),
                activated: RefCell::new(Vec::new()),
                raise_free_activated: RefCell::new(Vec::new()),
                raise_free_available: false,
            }
        }

        /// Makes `activate_app_without_raise` report success (PINV-48).
        fn with_raise_free_available(mut self) -> Self {
            self.raise_free_available = true;
            self
        }
    }

    impl WindowManager for FakeWindowManager {
        fn activate_app(&self, app: &AppIdentifier) -> Result<(), PolarizeError> {
            self.activated.borrow_mut().push(app.clone());
            Ok(())
        }

        fn resolve_target_rect(
            &self,
            _target: &ScreenshotTarget,
        ) -> Result<PixelRect, PolarizeError> {
            Ok(self.rect)
        }

        fn resolve_target_pid(
            &self,
            _target: &ScreenshotTarget,
        ) -> Result<Option<i32>, PolarizeError> {
            Ok(self.pid)
        }

        fn activate_app_without_raise(&self, app: &AppIdentifier) -> Result<bool, PolarizeError> {
            if self.raise_free_available {
                self.raise_free_activated.borrow_mut().push(app.clone());
            }
            Ok(self.raise_free_available)
        }

        fn resolve_app_pid(&self, _app: &AppIdentifier) -> Result<Option<i32>, PolarizeError> {
            Ok(self.pid)
        }
    }

    #[derive(Default)]
    struct FakeInputSynthesizer {
        clicks: RefCell<Vec<(crate::coords::PixelPoint, u8, Option<i32>)>>,
        typed: RefCell<Vec<String>>,
        pressed: RefCell<Vec<(NamedKey, Vec<Modifier>)>>,
    }

    impl FakeInputSynthesizer {
        /// The `PostPath` every method below reports: `Pid` when a pid
        /// was passed, `Global` otherwise. Shared so the three methods
        /// don't each repeat the same decision.
        fn post_path(&self, pid: Option<i32>) -> PostPath {
            if pid.is_some() {
                PostPath::Pid
            } else {
                PostPath::Global
            }
        }
    }

    impl InputSynthesizer for FakeInputSynthesizer {
        fn click_at_pixel(
            &self,
            point: crate::coords::PixelPoint,
            click_count: u8,
            pid: Option<i32>,
        ) -> Result<PostPath, PolarizeError> {
            self.clicks.borrow_mut().push((point, click_count, pid));
            Ok(self.post_path(pid))
        }

        fn type_text(&self, text: &str, pid: Option<i32>) -> Result<PostPath, PolarizeError> {
            self.typed.borrow_mut().push(text.to_string());
            Ok(self.post_path(pid))
        }

        fn press_key(
            &self,
            key: NamedKey,
            modifiers: &[Modifier],
            pid: Option<i32>,
        ) -> Result<PostPath, PolarizeError> {
            self.pressed.borrow_mut().push((key, modifiers.to_vec()));
            Ok(self.post_path(pid))
        }
    }

    struct FakeAccessibilityInspector {
        app_name: String,
        bundle_id: Option<String>,
        root: AxNode,
    }

    impl AccessibilityInspector for FakeAccessibilityInspector {
        fn describe(
            &self,
            _app: Option<&AppIdentifier>,
        ) -> Result<(crate::traits::ResolvedApp, AxNode), PolarizeError> {
            Ok((
                crate::traits::ResolvedApp {
                    name: self.app_name.clone(),
                    bundle_id: self.bundle_id.clone(),
                },
                self.root.clone(),
            ))
        }
    }

    #[derive(Default)]
    struct FakeScreenCapture {
        screen_calls: RefCell<Vec<Option<u32>>>,
        window_calls: RefCell<Vec<(AppIdentifier, Option<String>)>>,
    }

    impl ScreenCapture for FakeScreenCapture {
        fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError> {
            self.screen_calls.borrow_mut().push(display_id);
            Ok(CapturedImage {
                png_bytes: vec![1, 2, 3, 4],
                width: 10,
                height: 20,
            })
        }

        fn capture_window(
            &self,
            app: &AppIdentifier,
            window_title: Option<&str>,
        ) -> Result<CapturedImage, PolarizeError> {
            self.window_calls
                .borrow_mut()
                .push((app.clone(), window_title.map(|s| s.to_string())));
            Ok(CapturedImage {
                png_bytes: vec![5, 6, 7],
                width: 30,
                height: 40,
            })
        }
    }

    fn leaf_tree() -> AxNode {
        AxNode {
            role: "AXWindow".to_string(),
            label: Some("Untitled".to_string()),
            frame: NormalizedFrame {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            children: vec![],
            ..AxNode::default()
        }
    }

    // ---- perform_tap ---------------------------------------------------

    #[test]
    fn perform_tap_normalizes_center_fraction_to_pixel_center() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 2000.0,
            height: 1000.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.5,
            y: 0.5,
            target: None,
            click_count: None,
        };

        let response = perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(
            response,
            TapResponse {
                tapped: true,
                pixel_x: 1000.0,
                pixel_y: 500.0,
                post_path: PostPath::Global,
            }
        );
        assert_eq!(
            input.clicks.borrow().as_slice(),
            &[(
                crate::coords::PixelPoint {
                    x: 1000.0,
                    y: 500.0
                },
                1,
                None
            )]
        );
    }

    #[test]
    fn perform_tap_defaults_click_count_to_one() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.0,
            y: 0.0,
            target: None,
            click_count: None,
        };

        perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(input.clicks.borrow()[0].1, 1);
    }

    #[test]
    fn perform_tap_forwards_explicit_click_count() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.0,
            y: 0.0,
            target: None,
            click_count: Some(2),
        };

        perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(input.clicks.borrow()[0].1, 2);
    }

    #[test]
    fn perform_tap_adds_target_origin_to_the_global_pixel_point() {
        // A window that does not start at the global origin — e.g. a
        // window sitting at screen offset (100, 50) — must have that
        // offset added to the resolved pixel point. Regression test for
        // PINV-4's origin fix: before it, an `App`/`Window`-scoped tap
        // silently clicked window-relative coordinates instead of the
        // real global point, hitting whatever sat at that offset on the
        // primary display instead of the intended element.
        let wm = FakeWindowManager::with_origin(
            PixelPoint { x: 100.0, y: 50.0 },
            PixelSize {
                width: 2000.0,
                height: 1000.0,
            },
        );
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.5,
            y: 0.5,
            target: None,
            click_count: None,
        };

        let response = perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(
            response,
            TapResponse {
                tapped: true,
                pixel_x: 1100.0,
                pixel_y: 550.0,
                post_path: PostPath::Global,
            }
        );
        assert_eq!(
            input.clicks.borrow().as_slice(),
            &[(
                crate::coords::PixelPoint {
                    x: 1100.0,
                    y: 550.0
                },
                1,
                None
            )]
        );
    }

    #[test]
    fn perform_tap_posts_by_pid_when_a_target_pid_is_available() {
        let wm = FakeWindowManager::with_pid(
            PixelSize {
                width: 100.0,
                height: 100.0,
            },
            4242,
        );
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.0,
            y: 0.0,
            target: Some(ScreenshotTarget::App {
                app: AppIdentifier {
                    bundle_id: Some("com.apple.TextEdit".to_string()),
                    app_name: None,
                },
            }),
            click_count: None,
        };

        let response = perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(response.post_path, PostPath::Pid);
        assert_eq!(input.clicks.borrow()[0].2, Some(4242));
    }

    #[test]
    fn perform_tap_falls_back_to_global_when_no_target_names_an_app() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 0.0,
            y: 0.0,
            target: None,
            click_count: None,
        };

        let response = perform_tap(&wm, &input, &request).unwrap();

        assert_eq!(response.post_path, PostPath::Global);
        assert_eq!(input.clicks.borrow()[0].2, None);
    }

    #[test]
    fn perform_tap_rejects_out_of_range_fraction_without_calling_platform() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = TapRequest {
            x: 1.5,
            y: 0.5,
            target: None,
            click_count: None,
        };

        let err = perform_tap(&wm, &input, &request).unwrap_err();

        assert!(matches!(err, PolarizeError::Coord(_)));
        assert!(
            input.clicks.borrow().is_empty(),
            "platform must not be called on bad input"
        );
    }

    // ---- perform_describe -----------------------------------------------

    #[test]
    fn perform_describe_passes_app_through_and_shapes_response() {
        let inspector = FakeAccessibilityInspector {
            app_name: "TextEdit".to_string(),
            bundle_id: Some("com.apple.TextEdit".to_string()),
            root: leaf_tree(),
        };
        let request = DescribeRequest { app: None };

        let response = perform_describe(&inspector, &request).unwrap();

        assert_eq!(response.app_name, "TextEdit");
        assert_eq!(response.bundle_id.as_deref(), Some("com.apple.TextEdit"));
        assert_eq!(response.root, leaf_tree());
    }

    #[test]
    fn perform_describe_fills_formatted_from_ax_format_tree() {
        let root = leaf_tree();
        let inspector = FakeAccessibilityInspector {
            app_name: "TextEdit".to_string(),
            bundle_id: None,
            root: root.clone(),
        };
        let request = DescribeRequest { app: None };

        let response = perform_describe(&inspector, &request).unwrap();

        assert_eq!(response.formatted, crate::ax::format_tree(&root));
    }

    // ---- perform_screenshot -----------------------------------------------

    #[test]
    fn perform_screenshot_dispatches_screen_target_to_capture_screen() {
        let capture = FakeScreenCapture::default();
        let request = ScreenshotRequest {
            target: ScreenshotTarget::Screen {
                display_id: Some(2),
            },
        };

        let response = perform_screenshot(&capture, &request).unwrap();

        assert_eq!(capture.screen_calls.borrow().as_slice(), &[Some(2)]);
        assert!(capture.window_calls.borrow().is_empty());
        assert_eq!(response.decode_png_bytes().unwrap(), vec![1, 2, 3, 4]);
        assert_eq!((response.width, response.height), (10, 20));
    }

    #[test]
    fn perform_screenshot_dispatches_app_target_to_capture_window_with_no_title() {
        let capture = FakeScreenCapture::default();
        let app = AppIdentifier {
            bundle_id: Some("com.apple.Notes".to_string()),
            app_name: None,
        };
        let request = ScreenshotRequest {
            target: ScreenshotTarget::App { app: app.clone() },
        };

        perform_screenshot(&capture, &request).unwrap();

        assert_eq!(capture.window_calls.borrow().as_slice(), &[(app, None)]);
    }

    #[test]
    fn perform_screenshot_dispatches_window_target_with_title() {
        let capture = FakeScreenCapture::default();
        let app = AppIdentifier {
            bundle_id: None,
            app_name: Some("Safari".to_string()),
        };
        let request = ScreenshotRequest {
            target: ScreenshotTarget::Window {
                app: app.clone(),
                window_title: "Inbox".to_string(),
            },
        };

        perform_screenshot(&capture, &request).unwrap();

        assert_eq!(
            capture.window_calls.borrow().as_slice(),
            &[(app, Some("Inbox".to_string()))]
        );
    }

    // ---- perform_keyboard -----------------------------------------------

    #[test]
    fn perform_keyboard_type_dispatches_to_type_text() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: None,
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(
            response,
            KeyboardResponse {
                sent: true,
                activation_path: ActivationPath::None,
                post_path: PostPath::Global,
            }
        );
        assert_eq!(input.typed.borrow().as_slice(), &["hi".to_string()]);
        assert!(input.pressed.borrow().is_empty());
    }

    #[test]
    fn perform_keyboard_key_press_dispatches_to_press_key_with_modifiers() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = KeyboardRequest::KeyPress {
            key: NamedKey::Return,
            modifiers: vec![Modifier::Command],
            target: None,
        };

        perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(
            input.pressed.borrow().as_slice(),
            &[(NamedKey::Return, vec![Modifier::Command])]
        );
        assert!(input.typed.borrow().is_empty());
    }

    // ---- perform_keyboard target activation -----------------------------
    //
    // PINV-14: a `keyboard` request naming a `target` app activates that
    // app before posting any key event, so the input actually reaches it.

    #[test]
    fn perform_keyboard_activates_target_app_before_typing() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let target = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: Some(target.clone()),
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(wm.activated.borrow().as_slice(), &[target]);
        assert_eq!(input.typed.borrow().as_slice(), &["hi".to_string()]);
        assert_eq!(response.activation_path, ActivationPath::Raised);
    }

    #[test]
    fn perform_keyboard_activates_target_app_before_key_press() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let target = AppIdentifier {
            bundle_id: None,
            app_name: Some("Safari".to_string()),
        };
        let request = KeyboardRequest::KeyPress {
            key: NamedKey::Return,
            modifiers: vec![],
            target: Some(target.clone()),
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(wm.activated.borrow().as_slice(), &[target]);
        assert_eq!(response.activation_path, ActivationPath::Raised);
    }

    #[test]
    fn perform_keyboard_prefers_raise_free_activation_when_available() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        })
        .with_raise_free_available();
        let input = FakeInputSynthesizer::default();
        let target = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: Some(target.clone()),
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(wm.raise_free_activated.borrow().as_slice(), &[target]);
        assert!(
            wm.activated.borrow().is_empty(),
            "activate_app must not run when the raise-free path already succeeded"
        );
        assert_eq!(response.activation_path, ActivationPath::RaiseFree);
    }

    #[test]
    fn perform_keyboard_does_not_activate_anything_when_target_is_none() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: None,
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert!(wm.activated.borrow().is_empty());
        assert!(wm.raise_free_activated.borrow().is_empty());
        assert_eq!(response.activation_path, ActivationPath::None);
        assert_eq!(response.post_path, PostPath::Global);
    }

    // ---- perform_keyboard pid-post -----------------------------------
    //
    // PINV-49: the same pid-post decision table PINV-47 established for
    // `tap`, extended to `keyboard`'s key events.

    #[test]
    fn perform_keyboard_posts_by_pid_when_a_target_pid_is_available() {
        let wm = FakeWindowManager::with_pid(
            PixelSize {
                width: 100.0,
                height: 100.0,
            },
            4242,
        );
        let input = FakeInputSynthesizer::default();
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: Some(AppIdentifier {
                bundle_id: Some("com.apple.TextEdit".to_string()),
                app_name: None,
            }),
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(response.post_path, PostPath::Pid);
    }

    #[test]
    fn perform_keyboard_falls_back_to_global_when_no_target_names_an_app() {
        let wm = FakeWindowManager::new(PixelSize {
            width: 100.0,
            height: 100.0,
        });
        let input = FakeInputSynthesizer::default();
        let request = KeyboardRequest::Type {
            text: "hi".to_string(),
            target: None,
        };

        let response = perform_keyboard(&wm, &input, &request).unwrap();

        assert_eq!(response.post_path, PostPath::Global);
    }
}
