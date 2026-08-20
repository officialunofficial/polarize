//! `find_text`: the OCR fallback for apps whose accessibility tree
//! cannot answer.
//!
//! `describe` and `selector` read an app's `AXUIElement` tree. Games,
//! canvas UI, and some Electron wrappers publish almost nothing there.
//! `find_text` reads the pixels instead. It captures the same image
//! `screenshot` captures, runs Vision's `VNRecognizeTextRequest` over
//! it, and returns the position of the text a caller asked for.
//!
//! ## The split this module depends on
//!
//! Capture and recognition stay in two separate traits.
//! [`crate::traits::ScreenCapture`] returns the pixels. The new
//! [`crate::traits::TextRecognizer`] turns those pixels into text.
//! Neither one knows about the other, and this module composes them.
//! So every decision below — the confidence floor, the match test, the
//! order, the coordinate flip — runs under `cargo test` against fake
//! implementations of both traits.
//!
//! ## The coordinate flip (PINV-37)
//!
//! Vision reports a bounding box in a normalized space whose origin is
//! the **bottom** left. `y` grows upward. `polarize` reports a
//! [`NormalizedFrame`] whose origin is the **top** left, and `y` grows
//! downward (PINV-8). [`flip_to_top_left`] converts between them.
//!
//! A dropped flip is invisible in a response. Every number still lies
//! in `0.0..=1.0`, and `tap` still accepts it. The tap lands on the
//! mirror image of the right place. So the flip has its own invariant,
//! and its own exhaustive tests below.
//!
//! ## First call: about 27 seconds
//!
//! Vision compiles its text-recognition model once per OS version. The
//! first `find_text` call after an OS update pays that cost. Measured
//! on real hardware, it takes about 27 seconds. macOS caches the result,
//! and later calls take 114 to 128 ms in `Accurate` mode on a full
//! Retina screenshot. Tell the caller this. A first call that takes half
//! a minute otherwise looks like a hang.
//!
//! ## Threading
//!
//! `VNImageRequestHandler::performRequests` is synchronous. It needs no
//! run loop. It also blocks for 100 ms or more every call, and for about
//! 27 seconds on the first one. The MCP server must run this tool
//! through `tokio::task::spawn_blocking`, or it stalls the whole async
//! runtime.
//!
//! ## Permission
//!
//! This tool adds no new TCC surface. Vision needs no permission of its
//! own. The pixels come from the existing `ScreenCaptureKit` path, which
//! already holds Screen Recording. So `ToolKind::FindText` maps to
//! `PermissionKind::ScreenRecording`, the same permission `screenshot`
//! uses.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::NormalizedFrame;
use crate::error::PolarizeError;
use crate::schema::ScreenshotTarget;
use crate::traits::{ScreenCapture, TextRecognizer};

/// The confidence floor a request gets when it sets none.
///
/// Vision reports a confidence in `0.0..=1.0` for every line it reads.
/// It reads text out of textured backgrounds, window shadows, and image
/// content, and it reports those with a low confidence. A floor removes
/// most of that. This default is deliberately low: it drops the obvious
/// garbage, and it keeps faint but real UI text.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.3;

/// How hard Vision works on one image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionLevel {
    /// `VNRequestTextRecognitionLevelAccurate`. About 114 to 128 ms on a
    /// full Retina screenshot. This is Vision's own default, and it is
    /// the right one for UI text.
    #[default]
    Accurate,
    /// `VNRequestTextRecognitionLevelFast`. It trades accuracy for
    /// speed, and it misses small text.
    Fast,
}

/// What [`TextRecognizer::recognize_text`] should do with one image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecognizeOptions {
    pub level: RecognitionLevel,
    /// Vision's `usesLanguageCorrection`. It defaults to `false` here,
    /// unlike Vision's own default. Language correction rewrites what it
    /// reads into likelier words. That helps prose. It corrupts UI text,
    /// where `"Untitled 1"`, a file name, or an identifier is not a word
    /// at all.
    pub uses_language_correction: bool,
    /// BCP-47 language tags, e.g. `["en-US"]`. An empty list keeps
    /// Vision's own language choice.
    pub languages: Vec<String>,
}

/// A rectangle in **Vision's** normalized image space.
///
/// The origin is the bottom left corner, and `y` grows upward. Every
/// component lies in `0.0..=1.0`, although Vision may report a box that
/// reaches slightly outside it. [`flip_to_top_left`] converts one of
/// these to `polarize`'s own top-left space. Nothing outside this module
/// and `polarize-macos`'s `vision` module should hold this type.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VisionRect {
    /// The left edge.
    pub x: f64,
    /// The **bottom** edge.
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One word of a recognized line, with its own box.
///
/// `polarize-macos` reads these from `VNRecognizedText`'s
/// `boundingBoxForRange:error:`. The byte offsets address
/// [`RecognizedLine::text`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecognizedWord {
    pub byte_start: usize,
    pub byte_end: usize,
    pub bounds: VisionRect,
}

/// One line of text Vision recognized in an image.
#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedLine {
    /// The top candidate string for this line.
    pub text: String,
    /// The top candidate's confidence, in `0.0..=1.0`.
    pub confidence: f64,
    /// The whole line's box, in Vision's bottom-left space.
    pub bounds: VisionRect,
    /// Per-word boxes, when the recognizer supplied them. An empty list
    /// is normal: [`find_text`](self) then reports the whole line's box.
    pub words: Vec<RecognizedWord>,
}

/// How a request compares its text against one recognized line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextMatchMode {
    /// The line contains the request's text. This is the default,
    /// because OCR reads a whole label as one line.
    #[default]
    Contains,
    /// The whole line equals the request's text. Leading and trailing
    /// whitespace does not count, because OCR adds it.
    Exact,
}

/// A `find_text` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindTextRequest {
    /// The text to look for. It must not be empty.
    pub text: String,
    /// What to capture and read, exactly as `screenshot` scopes it.
    /// `None` means the main screen.
    #[serde(default)]
    pub target: Option<ScreenshotTarget>,
    /// Substring or whole-line comparison. Defaults to
    /// [`TextMatchMode::Contains`].
    #[serde(default)]
    pub mode: TextMatchMode,
    /// When `false`, the default, `"save"` matches `"Save"`.
    #[serde(default)]
    pub case_sensitive: bool,
    /// The lowest confidence a line may have and still match. `None`
    /// means [`DEFAULT_MIN_CONFIDENCE`]. It must lie in `0.0..=1.0`.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Which match to take when several lines match, counted from `0`
    /// in reading order. Defaults to `0`, the first match. This is the
    /// same rule [`crate::selector::ElementSelector::index`] uses.
    #[serde(default)]
    pub index: Option<usize>,
    /// Accuracy against speed. Defaults to [`RecognitionLevel::Accurate`].
    #[serde(default)]
    pub level: RecognitionLevel,
    /// Vision's `usesLanguageCorrection`. Defaults to `false`; see
    /// [`RecognizeOptions::uses_language_correction`].
    #[serde(default)]
    pub uses_language_correction: bool,
    /// BCP-47 language tags. An empty list keeps Vision's own choice.
    #[serde(default)]
    pub languages: Vec<String>,
}

impl FindTextRequest {
    /// A request that looks for `text` on the main screen, with every
    /// default applied.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            target: None,
            mode: TextMatchMode::default(),
            case_sensitive: false,
            min_confidence: None,
            index: None,
            level: RecognitionLevel::default(),
            uses_language_correction: false,
            languages: Vec::new(),
        }
    }

    /// What this request asks the recognizer to do.
    pub fn recognize_options(&self) -> RecognizeOptions {
        RecognizeOptions {
            level: self.level,
            uses_language_correction: self.uses_language_correction,
            languages: self.languages.clone(),
        }
    }
}

/// One line that matched, in `polarize`'s own coordinate space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextMatch {
    /// The whole recognized line, not only the part that matched.
    pub text: String,
    /// Vision's confidence for this line, in `0.0..=1.0`.
    pub confidence: f64,
    /// Where the match sits, with a **top-left** origin (PINV-37).
    pub frame: NormalizedFrame,
    /// The center of `frame`. Pass this straight to `tap`, with the
    /// same `target` this request used.
    pub center_x: f64,
    pub center_y: f64,
    /// This match's position in reading order, counted from `0`.
    pub index: usize,
    /// `true` when `frame` covers only the words the match spans.
    /// `false` when it covers the whole recognized line, because the
    /// recognizer supplied no word boxes.
    pub narrowed_to_words: bool,
}

/// The result of one `find_text` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindTextResponse {
    /// The match the request's `index` selected.
    pub matched: TextMatch,
    /// How many lines matched, after the confidence floor.
    pub match_count: usize,
    /// How many lines Vision recognized, before any filter.
    pub observation_count: usize,
    /// How many lines the confidence floor removed.
    pub below_confidence_count: usize,
    /// The floor this call applied.
    pub min_confidence: f64,
    /// The pixel size of the image OCR read. A caller needs it to turn
    /// `center_x`/`center_y` back into pixels.
    pub image_width: u32,
    pub image_height: u32,
}

/// Why a `find_text` call did not return one match.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FindTextError {
    /// The request named no text at all.
    #[error("find_text needs a non-empty text to look for")]
    EmptyQuery,

    /// `min_confidence` fell outside `0.0..=1.0`.
    #[error("min_confidence {value} is out of range 0.0..=1.0")]
    InvalidConfidence { value: f64 },

    /// No recognized line satisfied the request.
    #[error("no recognized text matches {query:?}; {observations} line(s) recognized{sample}")]
    NoMatch {
        query: String,
        observations: usize,
        /// A short list of what the OCR did read, already formatted.
        sample: String,
    },

    /// Lines matched, but fewer than the request's `index` needs.
    #[error("{query:?} matched {matches} line(s), so index {index} is out of range")]
    IndexOutOfRange {
        query: String,
        index: usize,
        matches: usize,
    },
}

impl From<FindTextError> for PolarizeError {
    /// `polarize-core`'s [`PolarizeError`] has no variant for a
    /// `find_text` refusal, and `error.rs` belongs to another agent this
    /// round. So a refusal travels as a `Platform` error for now. The
    /// message is right; only the variant is misleading. See the handoff
    /// note for the variant to add later.
    fn from(error: FindTextError) -> Self {
        PolarizeError::Platform(error.to_string())
    }
}

/// A word's position inside a string, in both byte and UTF-16 units.
///
/// `polarize-macos` needs the UTF-16 pair, because `NSRange` counts
/// UTF-16 code units. This module needs the byte pair, because Rust
/// slices count bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSpan {
    pub byte_start: usize,
    pub byte_end: usize,
    pub utf16_start: usize,
    pub utf16_len: usize,
}

/// Splits `text` into whitespace-separated words, and reports where each
/// one starts and ends.
pub fn word_spans(text: &str) -> Vec<WordSpan> {
    let mut spans = Vec::new();
    let mut utf16_index = 0usize;
    let mut open: Option<(usize, usize)> = None;
    for (byte_index, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some((byte_start, utf16_start)) = open.take() {
                spans.push(WordSpan {
                    byte_start,
                    byte_end: byte_index,
                    utf16_start,
                    utf16_len: utf16_index - utf16_start,
                });
            }
        } else if open.is_none() {
            open = Some((byte_index, utf16_index));
        }
        utf16_index += character.len_utf16();
    }
    if let Some((byte_start, utf16_start)) = open {
        spans.push(WordSpan {
            byte_start,
            byte_end: text.len(),
            utf16_start,
            utf16_len: utf16_index - utf16_start,
        });
    }
    spans
}

/// Replaces a non-finite component with `0.0`.
///
/// Vision reports real geometry, so this should never fire. A `NaN`
/// that did get through would travel all the way to a `tap` fraction,
/// where it fails a range check with a confusing message.
fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// # PINV-37: a Vision box is flipped into top-left space, never passed through
///
/// - Always: [`flip_to_top_left`] converts a [`VisionRect`] — origin
///   bottom-left, `y` growing upward — into a [`NormalizedFrame`] whose
///   origin is the top left and whose `y` grows downward. The rule is
///   `top = 1 - (bottom + height)`. It clamps the result into
///   `0.0..=1.0`, and it turns a non-finite component into `0.0`.
/// - Because: both spaces normalize to `0.0..=1.0`, so a missing flip
///   produces numbers that pass every range check `polarize` makes,
///   including `tap`'s (PINV-1). Nothing errors. The tap simply lands on
///   the mirror image of the right place, and a caller sees a click on
///   the wrong control with no failure at all.
/// - If violated: every `find_text` result taps the vertical mirror of
///   the text it found. A hit near the top of a window presses whatever
///   sits near the bottom.
pub fn flip_to_top_left(rect: VisionRect) -> NormalizedFrame {
    let left = finite(rect.x).clamp(0.0, 1.0);
    let right = (finite(rect.x) + finite(rect.width)).clamp(0.0, 1.0);
    let bottom = finite(rect.y).clamp(0.0, 1.0);
    let top = (finite(rect.y) + finite(rect.height)).clamp(0.0, 1.0);
    NormalizedFrame {
        x: left,
        // This one line is the whole flip. `top` is the box's upper edge
        // measured from the bottom, so `1 - top` measures it from the
        // top.
        y: (1.0 - top).clamp(0.0, 1.0),
        width: (right - left).max(0.0),
        height: (top - bottom).max(0.0),
    }
}

/// The smallest [`VisionRect`] that covers both inputs.
pub fn union(a: VisionRect, b: VisionRect) -> VisionRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let top = (a.y + a.height).max(b.y + b.height);
    VisionRect {
        x,
        y,
        width: right - x,
        height: top - y,
    }
}

/// Where `needle` sits inside `haystack`, as a byte range.
///
/// A case-insensitive search cannot fold the haystack first and reuse
/// the offset. Folding changes the byte length of some characters, so
/// an offset into the folded string does not address the original. This
/// grows a candidate slice from each character boundary instead, and
/// compares the folded candidate. An OCR line is short, so the cost does
/// not matter.
fn find_range(haystack: &str, needle: &str, case_sensitive: bool) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    if case_sensitive {
        return haystack
            .find(needle)
            .map(|start| (start, start + needle.len()));
    }
    let folded_needle = needle.to_lowercase();
    let needle_chars = folded_needle.chars().count();
    for (start, _) in haystack.char_indices() {
        for (offset, character) in haystack[start..].char_indices() {
            let end = start + offset + character.len_utf8();
            let candidate = haystack[start..end].to_lowercase();
            if candidate == folded_needle {
                return Some((start, end));
            }
            if candidate.chars().count() > needle_chars {
                break;
            }
        }
    }
    None
}

/// Where `query` sits inside `line`, as a byte range, or `None` when it
/// does not match at all.
pub fn matched_range(
    line: &str,
    query: &str,
    mode: TextMatchMode,
    case_sensitive: bool,
) -> Option<(usize, usize)> {
    match mode {
        TextMatchMode::Contains => find_range(line, query, case_sensitive),
        TextMatchMode::Exact => {
            let trimmed = line.trim();
            let same = if case_sensitive {
                trimmed == query
            } else {
                trimmed.to_lowercase() == query.to_lowercase()
            };
            if !same {
                return None;
            }
            let start = line.find(trimmed).unwrap_or(0);
            Some((start, start + trimmed.len()))
        }
    }
}

/// Everything one scan of a recognized image found.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchScan {
    /// The matches, in reading order, already numbered.
    pub matches: Vec<TextMatch>,
    /// How many lines the recognizer returned.
    pub observation_count: usize,
    /// How many lines the confidence floor removed.
    pub below_confidence_count: usize,
    /// The floor this scan applied.
    pub min_confidence: f64,
    /// Every line that survived the floor, in reading order. It answers
    /// "what did the OCR actually read" when nothing matched.
    pub recognized: Vec<String>,
}

/// # PINV-38: a `find_text` match is filtered, then ordered, then indexed
///
/// - Always: [`scan_lines`] drops every line below the confidence floor
///   first. It then keeps the lines that satisfy the request's match
///   mode. It orders what is left top to bottom by the recognized
///   line's own top edge, then left to right. Only then does
///   [`pick_match`] apply the request's `index`. An empty request text,
///   or a `min_confidence` outside `0.0..=1.0`, is rejected before any
///   of that.
/// - Because: Vision returns its observations in no order a caller can
///   rely on. `index` must name the same line on two calls against the
///   same screen, exactly as `ElementSelector::index` does for the
///   accessibility tree (PINV-15). Filtering after ordering, or
///   indexing before filtering, would move a caller's chosen match every
///   time a low-confidence line appears or disappears at the edge of the
///   screen.
/// - If violated: `index: 1` presses a different control on each call,
///   and a caller cannot see it happen, because both calls succeed.
pub fn scan_lines(
    lines: &[RecognizedLine],
    request: &FindTextRequest,
) -> Result<MatchScan, FindTextError> {
    let min_confidence = validate(request)?;

    // Step 1: the confidence floor.
    let kept: Vec<&RecognizedLine> = lines
        .iter()
        .filter(|line| line.confidence >= min_confidence)
        .collect();
    let below_confidence_count = lines.len() - kept.len();

    // Step 2: reading order, taken from each line's own box.
    let mut rows: Vec<(NormalizedFrame, &RecognizedLine)> = kept
        .into_iter()
        .map(|line| (flip_to_top_left(line.bounds), line))
        .collect();
    rows.sort_by(|(left, left_line), (right, right_line)| {
        left.y
            .total_cmp(&right.y)
            .then(left.x.total_cmp(&right.x))
            .then(left_line.text.cmp(&right_line.text))
    });

    // Step 3: the match test, and the narrowing to matched words.
    let mut matches = Vec::new();
    for (line_frame, line) in &rows {
        let Some(range) = matched_range(
            &line.text,
            &request.text,
            request.mode,
            request.case_sensitive,
        ) else {
            continue;
        };
        let (frame, narrowed_to_words) = match word_union(line, range) {
            Some(rect) => (flip_to_top_left(rect), true),
            None => (*line_frame, false),
        };
        matches.push(TextMatch {
            text: line.text.clone(),
            confidence: line.confidence,
            frame,
            center_x: frame.x + frame.width / 2.0,
            center_y: frame.y + frame.height / 2.0,
            index: matches.len(),
            narrowed_to_words,
        });
    }

    Ok(MatchScan {
        matches,
        observation_count: lines.len(),
        below_confidence_count,
        min_confidence,
        recognized: rows.iter().map(|(_, line)| line.text.clone()).collect(),
    })
}

/// The union of the boxes of every word the byte range touches, or
/// `None` when the line carries no word boxes at all.
fn word_union(line: &RecognizedLine, range: (usize, usize)) -> Option<VisionRect> {
    let (start, end) = range;
    let mut covered: Option<VisionRect> = None;
    for word in &line.words {
        if word.byte_start < end && word.byte_end > start {
            covered = Some(match covered {
                Some(rect) => union(rect, word.bounds),
                None => word.bounds,
            });
        }
    }
    covered
}

/// Up to eight recognized strings, formatted for an error message.
fn sample_of(recognized: &[String]) -> String {
    if recognized.is_empty() {
        return String::new();
    }
    let listed: Vec<String> = recognized
        .iter()
        .take(8)
        .map(|text| format!("{text:?}"))
        .collect();
    let more = if recognized.len() > listed.len() {
        ", …"
    } else {
        ""
    };
    format!(": {}{}", listed.join(", "), more)
}

/// Takes the match the request's `index` names. See PINV-38.
pub fn pick_match(scan: &MatchScan, request: &FindTextRequest) -> Result<TextMatch, FindTextError> {
    if scan.matches.is_empty() {
        return Err(FindTextError::NoMatch {
            query: request.text.clone(),
            observations: scan.observation_count,
            sample: sample_of(&scan.recognized),
        });
    }
    let index = request.index.unwrap_or(0);
    scan.matches
        .get(index)
        .cloned()
        .ok_or_else(|| FindTextError::IndexOutOfRange {
            query: request.text.clone(),
            index,
            matches: scan.matches.len(),
        })
}

/// Captures the requested target, reads its text, and returns the match
/// the request selects.
///
/// This is the whole `find_text` tool. `capture` is the same
/// [`ScreenCapture`] `screenshot` uses, so this call needs no permission
/// beyond Screen Recording. `recognizer` never sees a capture request,
/// and `capture` never sees a word of text.
///
/// Run this through `tokio::task::spawn_blocking`. It blocks for 100 ms
/// or more, and for about 27 seconds on the first call after an OS
/// update.
pub fn perform_find_text<C, R>(
    capture: &C,
    recognizer: &R,
    request: &FindTextRequest,
) -> Result<FindTextResponse, PolarizeError>
where
    C: ScreenCapture,
    R: TextRecognizer,
{
    // Reject a bad request before the platform does any work. A capture
    // costs real time, and OCR costs much more.
    validate(request)?;

    let target = request
        .target
        .clone()
        .unwrap_or(ScreenshotTarget::Screen { display_id: None });
    let image = match &target {
        ScreenshotTarget::Screen { display_id } => capture.capture_screen(*display_id)?,
        ScreenshotTarget::App { app } => capture.capture_window(app, None)?,
        ScreenshotTarget::Window { app, window_title } => {
            capture.capture_window(app, Some(window_title.as_str()))?
        }
    };

    let lines = recognizer.recognize_text(&image, &request.recognize_options())?;
    let scan = scan_lines(&lines, request)?;
    let matched = pick_match(&scan, request)?;
    Ok(FindTextResponse {
        matched,
        match_count: scan.matches.len(),
        observation_count: scan.observation_count,
        below_confidence_count: scan.below_confidence_count,
        min_confidence: scan.min_confidence,
        image_width: image.width,
        image_height: image.height,
    })
}

/// Checks the parts of a request that need no pixels at all, and
/// resolves the confidence floor it asks for.
fn validate(request: &FindTextRequest) -> Result<f64, FindTextError> {
    if request.text.trim().is_empty() {
        return Err(FindTextError::EmptyQuery);
    }
    let min_confidence = request.min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE);
    if !(0.0..=1.0).contains(&min_confidence) {
        return Err(FindTextError::InvalidConfidence {
            value: min_confidence,
        });
    }
    Ok(min_confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::{self, Fraction, PixelSize};
    use crate::schema::AppIdentifier;
    use crate::traits::CapturedImage;
    use std::cell::RefCell;

    // ---- helpers -----------------------------------------------------

    fn vision_rect(x: f64, y: f64, width: f64, height: f64) -> VisionRect {
        VisionRect {
            x,
            y,
            width,
            height,
        }
    }

    fn line(text: &str, confidence: f64, bounds: VisionRect) -> RecognizedLine {
        RecognizedLine {
            text: text.to_string(),
            confidence,
            bounds,
            words: Vec::new(),
        }
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{what}: expected {expected}, got {actual}"
        );
    }

    fn assert_frame(actual: NormalizedFrame, expected: [f64; 4], what: &str) {
        assert_close(actual.x, expected[0], &format!("{what}.x"));
        assert_close(actual.y, expected[1], &format!("{what}.y"));
        assert_close(actual.width, expected[2], &format!("{what}.width"));
        assert_close(actual.height, expected[3], &format!("{what}.height"));
    }

    // ---- fakes -------------------------------------------------------

    #[derive(Debug, Clone, PartialEq)]
    enum CaptureCall {
        Screen(Option<u32>),
        Window(AppIdentifier, Option<String>),
    }

    struct FakeCapture {
        image: CapturedImage,
        calls: RefCell<Vec<CaptureCall>>,
        fail: bool,
    }

    impl FakeCapture {
        fn new() -> Self {
            Self {
                image: CapturedImage {
                    png_bytes: vec![0x89, b'P', b'N', b'G'],
                    width: 2560,
                    height: 1600,
                },
                calls: RefCell::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }
    }

    impl ScreenCapture for FakeCapture {
        fn capture_screen(&self, display_id: Option<u32>) -> Result<CapturedImage, PolarizeError> {
            self.calls
                .borrow_mut()
                .push(CaptureCall::Screen(display_id));
            if self.fail {
                return Err(PolarizeError::Platform("capture refused".to_string()));
            }
            Ok(self.image.clone())
        }

        fn capture_window(
            &self,
            app: &AppIdentifier,
            window_title: Option<&str>,
        ) -> Result<CapturedImage, PolarizeError> {
            self.calls.borrow_mut().push(CaptureCall::Window(
                app.clone(),
                window_title.map(str::to_string),
            ));
            if self.fail {
                return Err(PolarizeError::Platform("capture refused".to_string()));
            }
            Ok(self.image.clone())
        }
    }

    struct FakeRecognizer {
        lines: Vec<RecognizedLine>,
        calls: RefCell<Vec<(CapturedImage, RecognizeOptions)>>,
    }

    impl FakeRecognizer {
        fn new(lines: Vec<RecognizedLine>) -> Self {
            Self {
                lines,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl TextRecognizer for FakeRecognizer {
        fn recognize_text(
            &self,
            image: &CapturedImage,
            options: &RecognizeOptions,
        ) -> Result<Vec<RecognizedLine>, PolarizeError> {
            self.calls
                .borrow_mut()
                .push((image.clone(), options.clone()));
            Ok(self.lines.clone())
        }
    }

    // ---- PINV-37: the bottom-left to top-left flip --------------------

    #[test]
    fn a_full_frame_box_flips_to_a_full_frame() {
        let frame = flip_to_top_left(vision_rect(0.0, 0.0, 1.0, 1.0));
        assert_frame(frame, [0.0, 0.0, 1.0, 1.0], "full frame");
    }

    #[test]
    fn a_box_at_the_bottom_of_the_image_flips_to_the_bottom() {
        // Vision: sitting on the bottom edge, one tenth tall.
        let frame = flip_to_top_left(vision_rect(0.0, 0.0, 1.0, 0.1));
        assert_frame(frame, [0.0, 0.9, 1.0, 0.1], "bottom strip");
    }

    #[test]
    fn a_box_at_the_top_of_the_image_flips_to_zero() {
        // Vision: touching the top edge, one tenth tall.
        let frame = flip_to_top_left(vision_rect(0.0, 0.9, 1.0, 0.1));
        assert_frame(frame, [0.0, 0.0, 1.0, 0.1], "top strip");
    }

    #[test]
    fn each_corner_flips_to_the_mirrored_corner() {
        let cases = [
            // (vision box, expected top-left frame, name)
            ([0.0, 0.0, 0.1, 0.1], [0.0, 0.9, 0.1, 0.1], "bottom left"),
            ([0.9, 0.0, 0.1, 0.1], [0.9, 0.9, 0.1, 0.1], "bottom right"),
            ([0.0, 0.9, 0.1, 0.1], [0.0, 0.0, 0.1, 0.1], "top left"),
            ([0.9, 0.9, 0.1, 0.1], [0.9, 0.0, 0.1, 0.1], "top right"),
        ];
        for (input, expected, name) in cases {
            let frame = flip_to_top_left(vision_rect(input[0], input[1], input[2], input[3]));
            assert_frame(frame, expected, name);
        }
    }

    #[test]
    fn the_x_axis_never_moves_in_a_flip() {
        let frame = flip_to_top_left(vision_rect(0.25, 0.5, 0.3, 0.2));
        assert_close(frame.x, 0.25, "x");
        assert_close(frame.width, 0.3, "width");
    }

    #[test]
    fn a_box_reaching_outside_the_unit_square_clamps() {
        let frame = flip_to_top_left(vision_rect(-0.2, -0.1, 0.5, 0.4));
        assert_frame(frame, [0.0, 0.7, 0.3, 0.3], "clamped low");

        let frame = flip_to_top_left(vision_rect(0.8, 0.8, 0.5, 0.5));
        assert_frame(frame, [0.8, 0.0, 0.2, 0.2], "clamped high");
    }

    #[test]
    fn a_non_finite_box_component_becomes_zero() {
        let frame = flip_to_top_left(vision_rect(f64::NAN, 0.0, f64::INFINITY, 0.2));
        assert!(frame.x.is_finite(), "x must be finite, got {}", frame.x);
        assert!(frame.y.is_finite(), "y must be finite, got {}", frame.y);
        assert!(frame.width.is_finite(), "width must be finite");
        assert!(frame.height.is_finite(), "height must be finite");
    }

    #[test]
    fn every_flipped_frame_center_is_a_valid_tap_fraction() {
        let size = PixelSize {
            width: 2560.0,
            height: 1600.0,
        };
        let boxes = [
            vision_rect(0.0, 0.0, 1.0, 1.0),
            vision_rect(0.0, 0.0, 0.01, 0.01),
            vision_rect(0.99, 0.99, 0.01, 0.01),
            vision_rect(-0.5, -0.5, 2.0, 2.0),
            vision_rect(0.5, 0.5, 0.0, 0.0),
        ];
        for rect in boxes {
            let frame = flip_to_top_left(rect);
            let fraction = Fraction {
                x: frame.x + frame.width / 2.0,
                y: frame.y + frame.height / 2.0,
            };
            assert!(
                coords::fraction_to_pixel(fraction, size).is_ok(),
                "flipped center {fraction:?} must be a valid tap fraction"
            );
        }
    }

    #[test]
    fn a_flip_moves_the_center_to_the_mirrored_center() {
        let rect = vision_rect(0.1, 0.2, 0.2, 0.2);
        let vision_center_y = rect.y + rect.height / 2.0;
        let frame = flip_to_top_left(rect);
        let frame_center_y = frame.y + frame.height / 2.0;
        assert_close(frame_center_y, 1.0 - vision_center_y, "mirrored center");
    }

    // ---- union of word boxes -----------------------------------------

    #[test]
    fn a_union_of_two_boxes_covers_both() {
        let joined = union(
            vision_rect(0.1, 0.5, 0.1, 0.05),
            vision_rect(0.3, 0.4, 0.2, 0.1),
        );
        assert_close(joined.x, 0.1, "union.x");
        assert_close(joined.y, 0.4, "union.y");
        assert_close(joined.width, 0.4, "union.width");
        assert_close(joined.height, 0.15, "union.height");
    }

    #[test]
    fn a_union_of_one_box_with_itself_is_that_box() {
        // Compared component by component: a union recomputes the width
        // from two edges, so it lands within one float step of the input.
        let rect = vision_rect(0.2, 0.3, 0.1, 0.4);
        let joined = union(rect, rect);
        assert_close(joined.x, rect.x, "union.x");
        assert_close(joined.y, rect.y, "union.y");
        assert_close(joined.width, rect.width, "union.width");
        assert_close(joined.height, rect.height, "union.height");
    }

    // ---- word spans ---------------------------------------------------

    #[test]
    fn word_spans_splits_on_whitespace() {
        let spans = word_spans("File Edit View");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].byte_start, 0);
        assert_eq!(spans[0].byte_end, 4);
        assert_eq!(spans[1].byte_start, 5);
        assert_eq!(spans[1].byte_end, 9);
        assert_eq!(spans[2].byte_start, 10);
        assert_eq!(spans[2].byte_end, 14);
    }

    #[test]
    fn word_spans_ignores_repeated_and_edge_whitespace() {
        let spans = word_spans("  Save   As  ");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].utf16_start, 2);
        assert_eq!(spans[0].utf16_len, 4);
        assert_eq!(spans[1].utf16_start, 9);
        assert_eq!(spans[1].utf16_len, 2);
    }

    #[test]
    fn word_spans_counts_utf16_units_for_non_ascii_text() {
        // "é" is two bytes and one UTF-16 unit. "😀" is four bytes and
        // two UTF-16 units. NSRange counts UTF-16 units.
        let spans = word_spans("café 😀x");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].byte_end, 5);
        assert_eq!(spans[0].utf16_len, 4);
        assert_eq!(spans[1].utf16_start, 5);
        assert_eq!(spans[1].utf16_len, 3);
    }

    #[test]
    fn word_spans_of_blank_text_is_empty() {
        assert!(word_spans("").is_empty());
        assert!(word_spans("   ").is_empty());
    }

    // ---- the match test ------------------------------------------------

    #[test]
    fn contains_matches_a_substring_case_insensitively_by_default() {
        let range = matched_range("Save changes", "save", TextMatchMode::Contains, false);
        assert_eq!(range, Some((0, 4)));
    }

    #[test]
    fn a_case_sensitive_contains_rejects_a_different_case() {
        assert_eq!(
            matched_range("Save changes", "save", TextMatchMode::Contains, true),
            None
        );
        assert_eq!(
            matched_range("Save changes", "Save", TextMatchMode::Contains, true),
            Some((0, 4))
        );
    }

    #[test]
    fn contains_reports_the_byte_range_inside_a_multibyte_line() {
        let range = matched_range("café latte", "latte", TextMatchMode::Contains, false);
        assert_eq!(range, Some((6, 11)));
    }

    #[test]
    fn exact_needs_the_whole_line_and_ignores_edge_whitespace() {
        assert_eq!(
            matched_range("  Save  ", "Save", TextMatchMode::Exact, false),
            Some((2, 6))
        );
        assert_eq!(
            matched_range("Save changes", "Save", TextMatchMode::Exact, false),
            None
        );
    }

    #[test]
    fn a_line_that_does_not_hold_the_text_never_matches() {
        assert_eq!(
            matched_range("Cancel", "Save", TextMatchMode::Contains, false),
            None
        );
    }

    // ---- PINV-38: filter, then order, then index -----------------------

    #[test]
    fn a_line_below_the_confidence_floor_never_matches() {
        let lines = vec![
            line("Save", 0.2, vision_rect(0.0, 0.5, 0.1, 0.05)),
            line("Save", 0.9, vision_rect(0.0, 0.2, 0.1, 0.05)),
        ];
        let mut request = FindTextRequest::new("Save");
        request.min_confidence = Some(0.5);
        let scan = scan_lines(&lines, &request).expect("scan");
        assert_eq!(scan.matches.len(), 1);
        assert_close(scan.matches[0].confidence, 0.9, "kept confidence");
        assert_eq!(scan.below_confidence_count, 1);
        assert_eq!(scan.observation_count, 2);
    }

    #[test]
    fn a_request_with_no_floor_uses_the_default_floor() {
        let lines = vec![
            line(
                "Save",
                DEFAULT_MIN_CONFIDENCE - 0.01,
                vision_rect(0.0, 0.5, 0.1, 0.05),
            ),
            line(
                "Save",
                DEFAULT_MIN_CONFIDENCE,
                vision_rect(0.0, 0.2, 0.1, 0.05),
            ),
        ];
        let scan = scan_lines(&lines, &FindTextRequest::new("Save")).expect("scan");
        assert_eq!(scan.matches.len(), 1);
        assert_close(scan.min_confidence, DEFAULT_MIN_CONFIDENCE, "floor");
        assert_eq!(scan.below_confidence_count, 1);
    }

    #[test]
    fn an_out_of_range_min_confidence_is_rejected() {
        let mut request = FindTextRequest::new("Save");
        request.min_confidence = Some(1.5);
        let err = scan_lines(&[], &request).unwrap_err();
        assert_eq!(err, FindTextError::InvalidConfidence { value: 1.5 });

        request.min_confidence = Some(-0.1);
        let err = scan_lines(&[], &request).unwrap_err();
        assert_eq!(err, FindTextError::InvalidConfidence { value: -0.1 });
    }

    #[test]
    fn an_empty_request_text_is_rejected() {
        let request = FindTextRequest::new("   ");
        assert_eq!(
            scan_lines(&[], &request).unwrap_err(),
            FindTextError::EmptyQuery
        );
        let request = FindTextRequest::new("");
        assert_eq!(
            scan_lines(&[], &request).unwrap_err(),
            FindTextError::EmptyQuery
        );
    }

    #[test]
    fn matches_come_back_top_to_bottom_then_left_to_right() {
        // Vision's y grows upward, so the highest y is the topmost line.
        let lines = vec![
            line("Save right", 0.9, vision_rect(0.6, 0.1, 0.2, 0.05)),
            line("Save top", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Save left", 0.9, vision_rect(0.1, 0.1, 0.2, 0.05)),
        ];
        let scan = scan_lines(&lines, &FindTextRequest::new("Save")).expect("scan");
        let order: Vec<&str> = scan.matches.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(order, vec!["Save top", "Save left", "Save right"]);
        assert_eq!(scan.matches[0].index, 0);
        assert_eq!(scan.matches[2].index, 2);
    }

    #[test]
    fn the_recognized_list_holds_every_kept_line_in_reading_order() {
        let lines = vec![
            line("Cancel", 0.9, vision_rect(0.6, 0.1, 0.2, 0.05)),
            line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Noise", 0.05, vision_rect(0.1, 0.5, 0.2, 0.05)),
        ];
        let scan = scan_lines(&lines, &FindTextRequest::new("Save")).expect("scan");
        assert_eq!(
            scan.recognized,
            vec!["Save".to_string(), "Cancel".to_string()]
        );
    }

    #[test]
    fn a_match_narrows_to_the_union_of_the_words_it_covers() {
        // "File Edit View" as one line, with a box per word.
        let mut recognized = line("File Edit View", 0.9, vision_rect(0.0, 0.9, 0.3, 0.04));
        recognized.words = vec![
            RecognizedWord {
                byte_start: 0,
                byte_end: 4,
                bounds: vision_rect(0.0, 0.9, 0.08, 0.04),
            },
            RecognizedWord {
                byte_start: 5,
                byte_end: 9,
                bounds: vision_rect(0.1, 0.9, 0.08, 0.04),
            },
            RecognizedWord {
                byte_start: 10,
                byte_end: 14,
                bounds: vision_rect(0.22, 0.9, 0.08, 0.04),
            },
        ];
        let scan = scan_lines(&[recognized], &FindTextRequest::new("Edit")).expect("scan");
        let matched = &scan.matches[0];
        assert!(matched.narrowed_to_words, "must narrow to the matched word");
        assert_frame(matched.frame, [0.1, 0.06, 0.08, 0.04], "narrowed frame");
        assert_close(matched.center_x, 0.14, "center_x");
    }

    #[test]
    fn a_match_spanning_two_words_unions_both_boxes() {
        let mut recognized = line("Save As", 0.9, vision_rect(0.0, 0.5, 0.2, 0.04));
        recognized.words = vec![
            RecognizedWord {
                byte_start: 0,
                byte_end: 4,
                bounds: vision_rect(0.0, 0.5, 0.09, 0.04),
            },
            RecognizedWord {
                byte_start: 5,
                byte_end: 7,
                bounds: vision_rect(0.12, 0.5, 0.08, 0.04),
            },
        ];
        let scan = scan_lines(&[recognized], &FindTextRequest::new("Save As")).expect("scan");
        assert_frame(
            scan.matches[0].frame,
            [0.0, 0.46, 0.2, 0.04],
            "unioned frame",
        );
    }

    #[test]
    fn a_match_without_word_boxes_reports_the_whole_line_box() {
        let lines = vec![line("Save changes", 0.9, vision_rect(0.1, 0.5, 0.3, 0.04))];
        let scan = scan_lines(&lines, &FindTextRequest::new("Save")).expect("scan");
        assert!(!scan.matches[0].narrowed_to_words);
        assert_frame(scan.matches[0].frame, [0.1, 0.46, 0.3, 0.04], "line frame");
    }

    #[test]
    fn a_match_center_sits_in_the_middle_of_its_frame() {
        let lines = vec![line("Save", 0.9, vision_rect(0.2, 0.6, 0.2, 0.1))];
        let scan = scan_lines(&lines, &FindTextRequest::new("Save")).expect("scan");
        let matched = &scan.matches[0];
        assert_close(matched.center_x, 0.3, "center_x");
        assert_close(matched.center_y, 0.35, "center_y");
    }

    // ---- picking one match --------------------------------------------

    #[test]
    fn pick_takes_the_first_match_when_the_request_names_no_index() {
        let lines = vec![
            line("Save A", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Save B", 0.9, vision_rect(0.1, 0.5, 0.2, 0.05)),
        ];
        let request = FindTextRequest::new("Save");
        let scan = scan_lines(&lines, &request).expect("scan");
        assert_eq!(pick_match(&scan, &request).expect("pick").text, "Save A");
    }

    #[test]
    fn pick_takes_the_index_the_request_names() {
        let lines = vec![
            line("Save A", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Save B", 0.9, vision_rect(0.1, 0.5, 0.2, 0.05)),
        ];
        let mut request = FindTextRequest::new("Save");
        request.index = Some(1);
        let scan = scan_lines(&lines, &request).expect("scan");
        assert_eq!(pick_match(&scan, &request).expect("pick").text, "Save B");
    }

    #[test]
    fn pick_rejects_an_index_past_the_last_match() {
        let lines = vec![line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))];
        let mut request = FindTextRequest::new("Save");
        request.index = Some(3);
        let scan = scan_lines(&lines, &request).expect("scan");
        let err = pick_match(&scan, &request).unwrap_err();
        assert_eq!(
            err,
            FindTextError::IndexOutOfRange {
                query: "Save".to_string(),
                index: 3,
                matches: 1,
            }
        );
    }

    #[test]
    fn pick_reports_what_the_ocr_did_read_when_nothing_matches() {
        let lines = vec![
            line("Cancel", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Quit", 0.9, vision_rect(0.1, 0.5, 0.2, 0.05)),
        ];
        let request = FindTextRequest::new("Save");
        let scan = scan_lines(&lines, &request).expect("scan");
        let err = pick_match(&scan, &request).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("Save"),
            "message names the query: {message}"
        );
        assert!(
            message.contains("Cancel"),
            "message lists what it read: {message}"
        );
        assert!(
            message.contains("Quit"),
            "message lists what it read: {message}"
        );
    }

    // ---- the whole tool ------------------------------------------------

    #[test]
    fn find_text_captures_the_main_screen_when_no_target_is_set() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))]);
        perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).expect("find");
        assert_eq!(*capture.calls.borrow(), vec![CaptureCall::Screen(None)]);
    }

    #[test]
    fn find_text_captures_the_display_the_request_names() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))]);
        let mut request = FindTextRequest::new("Save");
        request.target = Some(ScreenshotTarget::Screen {
            display_id: Some(7),
        });
        perform_find_text(&capture, &recognizer, &request).expect("find");
        assert_eq!(*capture.calls.borrow(), vec![CaptureCall::Screen(Some(7))]);
    }

    #[test]
    fn find_text_captures_one_window_when_the_request_names_one() {
        let app = AppIdentifier {
            bundle_id: Some("com.example.Game".to_string()),
            app_name: None,
        };
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))]);

        let mut request = FindTextRequest::new("Save");
        request.target = Some(ScreenshotTarget::App { app: app.clone() });
        perform_find_text(&capture, &recognizer, &request).expect("find");

        let mut request = FindTextRequest::new("Save");
        request.target = Some(ScreenshotTarget::Window {
            app: app.clone(),
            window_title: "Level 1".to_string(),
        });
        perform_find_text(&capture, &recognizer, &request).expect("find");

        assert_eq!(
            *capture.calls.borrow(),
            vec![
                CaptureCall::Window(app.clone(), None),
                CaptureCall::Window(app, Some("Level 1".to_string())),
            ]
        );
    }

    #[test]
    fn find_text_hands_the_captured_pixels_and_options_to_the_recognizer() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))]);
        let mut request = FindTextRequest::new("Save");
        request.level = RecognitionLevel::Fast;
        request.languages = vec!["en-US".to_string()];
        perform_find_text(&capture, &recognizer, &request).expect("find");

        let calls = recognizer.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, capture.image);
        assert_eq!(calls[0].1.level, RecognitionLevel::Fast);
        assert_eq!(calls[0].1.languages, vec!["en-US".to_string()]);
        assert!(
            !calls[0].1.uses_language_correction,
            "language correction stays off by default"
        );
    }

    #[test]
    fn find_text_returns_a_tap_ready_center_and_the_image_size() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.2, 0.6, 0.2, 0.1))]);
        let response =
            perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).expect("find");

        assert_eq!(response.image_width, 2560);
        assert_eq!(response.image_height, 1600);
        let size = PixelSize {
            width: f64::from(response.image_width),
            height: f64::from(response.image_height),
        };
        let pixel = coords::fraction_to_pixel(
            Fraction {
                x: response.matched.center_x,
                y: response.matched.center_y,
            },
            size,
        )
        .expect("a find_text center is always a valid tap fraction");
        assert_close(pixel.x, 0.3 * 2560.0, "pixel x");
        assert_close(pixel.y, 0.35 * 1600.0, "pixel y");
    }

    #[test]
    fn find_text_reports_every_count_it_used() {
        let capture = FakeCapture::new();
        let recognizer = FakeRecognizer::new(vec![
            line("Save A", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05)),
            line("Save B", 0.8, vision_rect(0.1, 0.5, 0.2, 0.05)),
            line("Save C", 0.1, vision_rect(0.1, 0.3, 0.2, 0.05)),
            line("Cancel", 0.9, vision_rect(0.1, 0.2, 0.2, 0.05)),
        ]);
        let response =
            perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).expect("find");
        assert_eq!(response.observation_count, 4);
        assert_eq!(response.below_confidence_count, 1);
        assert_eq!(response.match_count, 2);
        assert_close(response.min_confidence, DEFAULT_MIN_CONFIDENCE, "floor");
        assert_eq!(response.matched.text, "Save A");
    }

    #[test]
    fn find_text_errors_when_nothing_matches() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Cancel", 0.9, vision_rect(0.1, 0.9, 0.2, 0.05))]);
        let err =
            perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).unwrap_err();
        assert!(
            err.to_string().contains("no recognized text matches"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_text_never_captures_when_the_request_is_invalid() {
        let capture = FakeCapture::new();
        let recognizer = FakeRecognizer::new(Vec::new());
        let err = perform_find_text(&capture, &recognizer, &FindTextRequest::new("")).unwrap_err();
        assert!(err.to_string().contains("non-empty"), "unexpected: {err}");
        assert!(
            capture.calls.borrow().is_empty(),
            "an invalid request must never reach the platform"
        );
        assert!(recognizer.calls.borrow().is_empty());
    }

    #[test]
    fn find_text_passes_a_capture_failure_straight_back() {
        let capture = FakeCapture::failing();
        let recognizer = FakeRecognizer::new(Vec::new());
        let err =
            perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).unwrap_err();
        assert!(
            err.to_string().contains("capture refused"),
            "unexpected: {err}"
        );
        assert!(
            recognizer.calls.borrow().is_empty(),
            "a failed capture must not reach the recognizer"
        );
    }

    #[test]
    fn a_find_text_refusal_travels_as_a_platform_error_for_now() {
        let err: PolarizeError = FindTextError::EmptyQuery.into();
        assert!(matches!(err, PolarizeError::Platform(_)));
        assert!(err.to_string().contains("non-empty"));
    }

    // ---- the wire contract ---------------------------------------------

    #[test]
    fn a_find_text_request_round_trips_through_json() {
        let mut request = FindTextRequest::new("Save");
        request.target = Some(ScreenshotTarget::Screen {
            display_id: Some(2),
        });
        request.mode = TextMatchMode::Exact;
        request.case_sensitive = true;
        request.min_confidence = Some(0.5);
        request.index = Some(1);
        let json = serde_json::to_string(&request).expect("serialize");
        let back: FindTextRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, request);
    }

    #[test]
    fn a_find_text_request_needs_only_its_text() {
        let request: FindTextRequest =
            serde_json::from_str(r#"{"text":"Save"}"#).expect("deserialize");
        assert_eq!(request, FindTextRequest::new("Save"));
    }

    #[test]
    fn a_find_text_response_round_trips_through_json() {
        let capture = FakeCapture::new();
        let recognizer =
            FakeRecognizer::new(vec![line("Save", 0.9, vision_rect(0.2, 0.6, 0.2, 0.1))]);
        let response =
            perform_find_text(&capture, &recognizer, &FindTextRequest::new("Save")).expect("find");
        let json = serde_json::to_string(&response).expect("serialize");
        let back: FindTextResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, response);
    }
}
