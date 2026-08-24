//! Manual re-verification tool for PINV-49's correction in
//! `docs/INVARIANTS.md`: a pid-posted keystroke reaches its target's
//! focused field with no activation call at all. `polarize-macos`'s
//! native calls have no automated coverage (see the crate-level docs),
//! so this is the invariant's actual check — not a throwaway.
//!
//! Posts text straight through `MacInputSynthesizer::type_text` with a
//! pid, bypassing `orchestrate::perform_keyboard` entirely. No
//! `activate_app` or `activate_app_without_raise` call runs first.
//!
//! Usage: launch a target app (e.g. TextEdit) with a document window
//! focused, then bring a *different* app to the front. Find the
//! target's pid (`pgrep -x TextEdit`), then run:
//!
//! ```sh
//! cargo run -p polarize-macos --example bg_keyboard_probe -- <pid> [text]
//! ```
//!
//! Expected observation: the frontmost app is unchanged before and
//! after (check with `mcp__polarize__frontmost_app`, or `osascript -e
//! 'tell application "System Events" to get name of first application
//! process whose frontmost is true'`), and the posted text appears in
//! the target's focused field regardless. If activation turns out to be
//! necessary on some future macOS release, this probe is how a human
//! re-confirms it and PINV-49 needs correcting back.

use polarize_core::traits::InputSynthesizer;
use polarize_macos::input::MacInputSynthesizer;

fn main() {
    let mut args = std::env::args().skip(1);
    let pid: i32 = args
        .next()
        .expect("usage: bg_keyboard_probe <pid> [text]")
        .parse()
        .expect("pid must be an integer");
    let text = args.next().unwrap_or_else(|| "BG-PROBE".to_string());

    let synth = MacInputSynthesizer;
    match synth.type_text(&text, Some(pid)) {
        Ok(path) => println!("posted {text:?} to pid {pid} via {path:?}"),
        Err(err) => {
            eprintln!("error posting to pid {pid}: {err:?}");
            std::process::exit(1);
        }
    }
}
