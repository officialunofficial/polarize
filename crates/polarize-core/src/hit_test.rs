//! The `hit_test_at_point` tool: it reports the element that really sits
//! under one point.
//!
//! `describe` starts from an element and reports where it is.
//! [`perform_hit_test`] starts from a point and reports which element is
//! there. macOS answers the question itself, through
//! `AXUIElementCopyElementAtPosition`. So the answer respects the
//! window order the user sees. An element that another window covers
//! never comes back.
//!
//! ## Why a caller wants this
//!
//! A `tap` posts a click at a point. The caller cannot see the screen,
//! so it cannot know what the click will reach. A hit test answers that
//! first. The caller compares the reported element against the element
//! it means to press. Then it taps, or it reports the difference.
//!
//! That preflight only works when both tools read one request the same
//! way. [`perform_hit_test`] and [`crate::orchestrate::perform_tap`]
//! share the whole coordinate path for that reason. See PINV-32.
//!
//! ## Why the reported element carries no children
//!
//! macOS already returns the deepest element it hit. The children of
//! that element do not cover the point, or the hit test would have
//! returned one of them instead. A subtree would also make one response
//! carry a whole web view. `describe` is the tool for a subtree. See
//! PINV-33.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;
use crate::coords::{self, Fraction, PixelPoint};
use crate::error::PolarizeError;
use crate::schema::{AppIdentifier, ScreenshotTarget};
use crate::traits::{HitTester, WindowManager};

/// A `hit_test_at_point` tool call.
///
/// The coordinate contract matches [`crate::schema::TapRequest`] exactly.
/// `x` and `y` are fractions of the target in `0.0..=1.0`. `target`
/// scopes the fraction to a screen or a window. `None` means the main
/// screen. A caller preflights a tap by sending the same three fields to
/// both tools (PINV-32).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HitTestRequest {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub target: Option<ScreenshotTarget>,
}

/// The result of a `hit_test_at_point` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HitTestResponse {
    /// The app the hit test read, as the platform resolved it.
    pub app_name: String,
    /// The resolved app's bundle id, when it publishes one.
    #[serde(default)]
    pub bundle_id: Option<String>,
    /// The global pixel point the fraction resolved to. A `tap` with the
    /// same request clicks this exact point (PINV-32).
    pub pixel_x: f64,
    /// See [`Self::pixel_x`].
    pub pixel_y: f64,
    /// The element under the point. `None` means macOS reports no
    /// element there. Its `children` list is always empty (PINV-33).
    #[serde(default)]
    pub element: Option<AxNode>,
}

/// The app a target names, or `None` for a whole screen.
///
/// A `Screen` target names no app, so the hit test reads the app the
/// platform picks. See [`HitTester::element_at_pixel`].
pub fn target_app(target: &ScreenshotTarget) -> Option<&AppIdentifier> {
    match target {
        ScreenshotTarget::Screen { .. } => None,
        ScreenshotTarget::App { app } => Some(app),
        ScreenshotTarget::Window { app, .. } => Some(app),
    }
}

/// # PINV-32: a hit test and a tap resolve one request to one pixel point
///
/// - Always: [`perform_hit_test`] converts `request.x`/`request.y` the
///   same way [`crate::orchestrate::perform_tap`] does. It resolves the
///   target through [`WindowManager::resolve_target_rect`]. It converts
///   the fraction against that rect's `size` through
///   [`coords::fraction_to_pixel`]. It adds that rect's `origin`. Only
///   the resolved global pixel point reaches [`HitTester`]. The reported
///   `pixel_x`/`pixel_y` equal the point a tap of the same request
///   clicks.
/// - Because: the caller uses one tool to preflight the other. It reads
///   the element under a point, compares it with the element it wants,
///   and then taps that same point. Any difference between the two
///   coordinate paths breaks that comparison without any error. The
///   preflight would then approve a click on another element.
/// - If violated: a caller confirms the right element, taps, and hits
///   something else. The tool reports success both times.
///
/// # PINV-33: a hit test reports one element, never a subtree
///
/// - Always: [`perform_hit_test`] clears the `children` list of the node
///   it reports.
/// - Because: macOS returns the deepest element under the point. A
///   caller asked "what is here", not "what does this contain".
/// - If violated: one hit test on a web view returns thousands of nodes.
pub fn perform_hit_test<W, H>(
    window_manager: &W,
    hit_tester: &H,
    request: &HitTestRequest,
) -> Result<HitTestResponse, PolarizeError>
where
    W: WindowManager,
    H: HitTester,
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
    let pixel = PixelPoint {
        x: rect.origin.x + local.x,
        y: rect.origin.y + local.y,
    };
    let (resolved, element) = hit_tester.element_at_pixel(target_app(&target), pixel)?;
    Ok(HitTestResponse {
        app_name: resolved.name,
        bundle_id: resolved.bundle_id,
        pixel_x: pixel.x,
        pixel_y: pixel.y,
        // PINV-33: one element, never a subtree.
        element: element.map(|node| AxNode {
            children: Vec::new(),
            ..node
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::NormalizedFrame;
    use crate::coords::{PixelRect, PixelSize};
    use crate::orchestrate::perform_tap;
    use crate::schema::{Modifier, NamedKey, TapRequest};
    use crate::traits::{InputSynthesizer, ResolvedApp};
    use std::cell::RefCell;

    // ---- fakes -------------------------------------------------------

    struct FakeWindowManager {
        rect: PixelRect,
        resolved: RefCell<Vec<ScreenshotTarget>>,
    }

    impl FakeWindowManager {
        fn new(origin: PixelPoint, size: PixelSize) -> Self {
            Self {
                rect: PixelRect { origin, size },
                resolved: RefCell::new(Vec::new()),
            }
        }
    }

    impl WindowManager for FakeWindowManager {
        fn activate_app(&self, _app: &AppIdentifier) -> Result<(), PolarizeError> {
            Ok(())
        }

        fn resolve_target_rect(
            &self,
            target: &ScreenshotTarget,
        ) -> Result<PixelRect, PolarizeError> {
            self.resolved.borrow_mut().push(target.clone());
            Ok(self.rect)
        }

        fn resolve_target_pid(
            &self,
            _target: &ScreenshotTarget,
        ) -> Result<Option<i32>, PolarizeError> {
            // Not exercised here: this module's tests are about pixel
            // agreement between `hit_test` and `tap` (PINV-32), not
            // about the pid-post path (PINV-47).
            Ok(None)
        }
    }

    #[derive(Default)]
    struct FakeHitTester {
        node: Option<AxNode>,
        calls: RefCell<Vec<(Option<AppIdentifier>, PixelPoint)>>,
    }

    impl FakeHitTester {
        fn with_node(node: AxNode) -> Self {
            Self {
                node: Some(node),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HitTester for FakeHitTester {
        fn element_at_pixel(
            &self,
            app: Option<&AppIdentifier>,
            point: PixelPoint,
        ) -> Result<(ResolvedApp, Option<AxNode>), PolarizeError> {
            self.calls.borrow_mut().push((app.cloned(), point));
            Ok((
                ResolvedApp {
                    name: "TextEdit".to_string(),
                    bundle_id: Some("com.apple.TextEdit".to_string()),
                },
                self.node.clone(),
            ))
        }
    }

    #[derive(Default)]
    struct FakeInputSynthesizer {
        clicks: RefCell<Vec<(PixelPoint, u8)>>,
    }

    impl InputSynthesizer for FakeInputSynthesizer {
        fn click_at_pixel(
            &self,
            point: PixelPoint,
            click_count: u8,
            _pid: Option<i32>,
        ) -> Result<crate::schema::PostPath, PolarizeError> {
            self.clicks.borrow_mut().push((point, click_count));
            Ok(crate::schema::PostPath::Global)
        }

        fn type_text(&self, _text: &str) -> Result<(), PolarizeError> {
            Ok(())
        }

        fn press_key(&self, _key: NamedKey, _modifiers: &[Modifier]) -> Result<(), PolarizeError> {
            Ok(())
        }
    }

    // ---- helpers -----------------------------------------------------

    const ORIGIN: PixelPoint = PixelPoint { x: 0.0, y: 0.0 };
    const SIZE: PixelSize = PixelSize {
        width: 1920.0,
        height: 1080.0,
    };

    fn button() -> AxNode {
        AxNode {
            role: "AXButton".to_string(),
            label: Some("Save".to_string()),
            subrole: Some("AXCloseButton".to_string()),
            identifier: Some("save-button".to_string()),
            actions: vec!["AXPress".to_string()],
            frame: NormalizedFrame {
                x: 0.5,
                y: 0.5,
                width: 0.1,
                height: 0.05,
            },
            ..AxNode::default()
        }
    }

    fn app(name: &str) -> AppIdentifier {
        AppIdentifier {
            bundle_id: None,
            app_name: Some(name.to_string()),
        }
    }

    fn request(x: f64, y: f64, target: Option<ScreenshotTarget>) -> HitTestRequest {
        HitTestRequest { x, y, target }
    }

    // ---- coordinate resolution (PINV-32) -----------------------------

    #[test]
    fn hit_test_resolves_a_fraction_against_the_target_size() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let response = perform_hit_test(&windows, &tester, &request(0.5, 0.5, None)).unwrap();
        assert_eq!((response.pixel_x, response.pixel_y), (960.0, 540.0));
        assert_eq!(
            tester.calls.borrow()[0].1,
            PixelPoint { x: 960.0, y: 540.0 }
        );
    }

    #[test]
    fn hit_test_adds_the_target_origin() {
        let windows = FakeWindowManager::new(PixelPoint { x: 100.0, y: 200.0 }, SIZE);
        let tester = FakeHitTester::with_node(button());
        let response = perform_hit_test(&windows, &tester, &request(0.5, 0.5, None)).unwrap();
        assert_eq!((response.pixel_x, response.pixel_y), (1060.0, 740.0));
        assert_eq!(
            tester.calls.borrow()[0].1,
            PixelPoint {
                x: 1060.0,
                y: 740.0
            }
        );
    }

    /// PINV-32. A caller preflights a tap with a hit test, so both tools
    /// must resolve one request to one point.
    #[test]
    fn a_hit_test_and_a_tap_resolve_one_request_to_one_pixel_point() {
        let target = ScreenshotTarget::App { app: app("Uno") };
        for (x, y) in [(0.0, 0.0), (0.25, 0.75), (0.5, 0.5), (1.0, 1.0)] {
            let origin = PixelPoint { x: 37.0, y: 129.0 };
            let size = PixelSize {
                width: 801.0,
                height: 633.0,
            };

            let hit_windows = FakeWindowManager::new(origin, size);
            let tester = FakeHitTester::with_node(button());
            let hit = perform_hit_test(&hit_windows, &tester, &request(x, y, Some(target.clone())))
                .unwrap();

            let tap_windows = FakeWindowManager::new(origin, size);
            let input = FakeInputSynthesizer::default();
            let tap = perform_tap(
                &tap_windows,
                &input,
                &TapRequest {
                    x,
                    y,
                    target: Some(target.clone()),
                    click_count: None,
                },
            )
            .unwrap();

            assert_eq!((hit.pixel_x, hit.pixel_y), (tap.pixel_x, tap.pixel_y));
            let clicked = input.clicks.borrow()[0].0;
            let tested = tester.calls.borrow()[0].1;
            assert_eq!(tested, clicked, "hit test and tap disagree at ({x}, {y})");
        }
    }

    #[test]
    fn hit_test_rejects_an_out_of_range_fraction_before_the_platform_runs() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let err = perform_hit_test(&windows, &tester, &request(1.4, 0.5, None)).unwrap_err();
        assert!(matches!(err, PolarizeError::Coord(_)));
        assert!(tester.calls.borrow().is_empty());
    }

    // ---- target scoping ----------------------------------------------

    #[test]
    fn hit_test_defaults_to_the_main_screen() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        perform_hit_test(&windows, &tester, &request(0.5, 0.5, None)).unwrap();
        assert_eq!(
            windows.resolved.borrow()[0],
            ScreenshotTarget::Screen { display_id: None }
        );
        assert_eq!(tester.calls.borrow()[0].0, None);
    }

    #[test]
    fn hit_test_scopes_the_read_to_an_app_target() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let target = ScreenshotTarget::App { app: app("Uno") };
        perform_hit_test(&windows, &tester, &request(0.5, 0.5, Some(target))).unwrap();
        assert_eq!(tester.calls.borrow()[0].0, Some(app("Uno")));
    }

    #[test]
    fn hit_test_scopes_the_read_to_a_window_targets_app() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let target = ScreenshotTarget::Window {
            app: app("Uno"),
            window_title: "Untitled".to_string(),
        };
        perform_hit_test(&windows, &tester, &request(0.5, 0.5, Some(target))).unwrap();
        assert_eq!(tester.calls.borrow()[0].0, Some(app("Uno")));
    }

    #[test]
    fn a_screen_target_names_no_app() {
        assert_eq!(
            target_app(&ScreenshotTarget::Screen {
                display_id: Some(2)
            }),
            None
        );
    }

    // ---- response shape (PINV-33) ------------------------------------

    /// PINV-33. The tool answers "what is here", not "what does this
    /// contain".
    #[test]
    fn hit_test_reports_one_element_without_its_children() {
        let parent = AxNode {
            role: "AXGroup".to_string(),
            children: vec![button(), button()],
            ..AxNode::default()
        };
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(parent);
        let response = perform_hit_test(&windows, &tester, &request(0.5, 0.5, None)).unwrap();
        let element = response.element.unwrap();
        assert_eq!(element.role, "AXGroup");
        assert!(element.children.is_empty());
    }

    #[test]
    fn hit_test_keeps_the_attributes_a_caller_compares() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let element = perform_hit_test(&windows, &tester, &request(0.5, 0.5, None))
            .unwrap()
            .element
            .unwrap();
        assert_eq!(element.role, "AXButton");
        assert_eq!(element.subrole.as_deref(), Some("AXCloseButton"));
        assert_eq!(element.label.as_deref(), Some("Save"));
        assert_eq!(element.identifier.as_deref(), Some("save-button"));
        assert!(element.enabled);
        assert_eq!(element.actions, vec!["AXPress".to_string()]);
        assert_eq!(element.frame.x, 0.5);
    }

    #[test]
    fn hit_test_reports_an_empty_point_as_no_element() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::default();
        let response = perform_hit_test(&windows, &tester, &request(0.1, 0.1, None)).unwrap();
        assert_eq!(response.element, None);
        assert_eq!(response.app_name, "TextEdit");
    }

    #[test]
    fn hit_test_reports_the_app_the_platform_resolved() {
        let windows = FakeWindowManager::new(ORIGIN, SIZE);
        let tester = FakeHitTester::with_node(button());
        let response = perform_hit_test(&windows, &tester, &request(0.5, 0.5, None)).unwrap();
        assert_eq!(response.app_name, "TextEdit");
        assert_eq!(response.bundle_id.as_deref(), Some("com.apple.TextEdit"));
    }

    #[test]
    fn hit_test_request_and_response_round_trip() {
        let request = request(0.25, 0.75, Some(ScreenshotTarget::App { app: app("Uno") }));
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<HitTestRequest>(&json).unwrap(),
            request
        );

        let response = HitTestResponse {
            app_name: "Uno".to_string(),
            bundle_id: None,
            pixel_x: 10.0,
            pixel_y: 20.0,
            element: Some(button()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<HitTestResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn a_request_without_a_target_deserializes() {
        let request: HitTestRequest = serde_json::from_str(r#"{"x":0.5,"y":0.5}"#).unwrap();
        assert_eq!(request.target, None);
    }
}
