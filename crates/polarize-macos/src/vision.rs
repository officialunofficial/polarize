//! [`TextRecognizer`] over Vision's `VNRecognizeTextRequest`.
//!
//! This module reads text out of an image `polarize-core` already
//! captured. It never captures anything itself. It holds no
//! ScreenCaptureKit code, and it asks for no permission. The pixels
//! arrive as encoded PNG bytes in a [`CapturedImage`], and
//! `VNImageRequestHandler` decodes them itself through
//! `initWithData:options:`. See [`polarize_core::find_text`] for the
//! logic that composes this trait with [`ScreenCapture`].
//!
//! [`ScreenCapture`]: polarize_core::traits::ScreenCapture
//!
//! ## The first call takes about 27 seconds
//!
//! macOS compiles the text-recognition model once per OS version. The
//! first `find_text` call after an OS update pays that cost, and it
//! takes about 27 seconds. macOS caches the result. Later calls take 114
//! to 128 ms in `Accurate` mode on a full Retina screenshot. The server
//! must say so, and must run this through `spawn_blocking`: every call
//! here blocks the calling thread.
//!
//! ## Permission
//!
//! Vision needs no TCC permission of its own. `crate::capture` already
//! holds Screen Recording, and it preflights it before every capture
//! (PINV-10). This module adds no new permission surface, so it runs no
//! preflight of its own.
//!
//! ## What is, and is not, verified
//!
//! No OCR has run in this environment, not once. There is no macOS
//! session here, no display, and no Screen Recording permission to
//! grant. Every call below is compile- and type-checked against
//! `aarch64-apple-darwin` and nothing more. A human on a real macOS
//! session must confirm the results, and especially the vertical axis:
//! see PINV-37 in `docs/INVARIANTS.md`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSRange, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText, VNRequest,
    VNRequestTextRecognitionLevel,
};
use polarize_core::error::PolarizeError;
use polarize_core::find_text::{
    RecognitionLevel, RecognizeOptions, RecognizedLine, RecognizedWord, VisionRect, word_spans,
};
use polarize_core::traits::{CapturedImage, TextRecognizer};

/// `TextRecognizer` implementation over Vision.
#[derive(Debug, Default)]
pub struct MacTextRecognizer;

impl TextRecognizer for MacTextRecognizer {
    fn recognize_text(
        &self,
        image: &CapturedImage,
        options: &RecognizeOptions,
    ) -> Result<Vec<RecognizedLine>, PolarizeError> {
        let data = NSData::with_bytes(&image.png_bytes);
        let no_options: Retained<NSDictionary<VNImageOption, AnyObject>> = NSDictionary::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &no_options,
        );

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(match options.level {
            RecognitionLevel::Accurate => VNRequestTextRecognitionLevel::Accurate,
            RecognitionLevel::Fast => VNRequestTextRecognitionLevel::Fast,
        });
        request.setUsesLanguageCorrection(options.uses_language_correction);
        if !options.languages.is_empty() {
            let languages: Vec<Retained<NSString>> = options
                .languages
                .iter()
                .map(|language| NSString::from_str(language))
                .collect();
            request.setRecognitionLanguages(&NSArray::from_retained_slice(&languages));
        }

        let requests: Retained<NSArray<VNRequest>> =
            NSArray::from_slice(&[request.as_super().as_super()]);
        // This blocks. It is the 100 ms — or 27 second — call.
        handler.performRequests_error(&requests).map_err(|error| {
            PolarizeError::Platform(format!("Vision text recognition failed: {error}"))
        })?;

        // An image with no text at all reports no results. That is a
        // normal answer, not a failure: `find_text` turns an empty list
        // into its own "nothing matched" error, which names the tool.
        let Some(results) = request.results() else {
            return Ok(Vec::new());
        };

        let mut lines = Vec::new();
        for observation in results.to_vec() {
            let candidates = observation.topCandidates(1);
            let Some(candidate) = candidates.to_vec().into_iter().next() else {
                continue;
            };
            let text = candidate.string().to_string();
            if text.trim().is_empty() {
                continue;
            }
            // SAFETY: `boundingBox` reads a value property of the
            // observation. Vision fills it in for every observation it
            // returns.
            let bounds = to_vision_rect(unsafe { observation.boundingBox() });
            let words = word_boxes(&candidate, &text);
            lines.push(RecognizedLine {
                text,
                confidence: f64::from(candidate.confidence()),
                bounds,
                words,
            });
        }
        Ok(lines)
    }
}

/// Copies a `CGRect` into `polarize-core`'s [`VisionRect`].
///
/// The rectangle keeps Vision's own space: the origin is the bottom-left
/// corner, and `y` grows upward. `polarize-core` flips it (PINV-37).
/// Nothing here may flip it first.
fn to_vision_rect(rect: CGRect) -> VisionRect {
    VisionRect {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

/// One box per word of `text`, read from `candidate`.
///
/// `boundingBoxForRange:error:` counts UTF-16 code units, which is why
/// [`word_spans`] reports them. A word whose box Vision refuses is
/// dropped, not reported as an error: `polarize-core` then falls back to
/// the whole line's box, which is correct, only wider.
fn word_boxes(candidate: &VNRecognizedText, text: &str) -> Vec<RecognizedWord> {
    let mut words = Vec::new();
    for span in word_spans(text) {
        let range = NSRange::new(span.utf16_start, span.utf16_len);
        // SAFETY: `range` addresses `text`, which is this candidate's own
        // string, so it never runs past the end.
        let Ok(rectangle) = (unsafe { candidate.boundingBoxForRange_error(range) }) else {
            continue;
        };
        // SAFETY: as above — a returned observation always carries a box.
        let bounds = to_vision_rect(unsafe { rectangle.boundingBox() });
        words.push(RecognizedWord {
            byte_start: span.byte_start,
            byte_end: span.byte_end,
            bounds,
        });
    }
    words
}
