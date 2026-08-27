# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/officialunofficial/polarize/releases/tag/v0.4.0) - 2026-08-27

### Features

- publish @unooo/polarize to npm with trusted publishing

## [0.3.0](https://github.com/officialunofficial/polarize/releases/tag/v0.3.0) - 2026-08-24

### Bug Fixes

- stop release-plz and dist racing to create the same GitHub Release,
  and cache CI's cargo deps
- keyboard skips activation when a target's pid resolves
- bound ScreenCaptureKit's completion wait, so it can't hang forever

### Features

- give polarize a real app bundle and its own TCC identity

### Documentation

- cut the build-history narrative from the README, keep only the
  facts

## [0.2.0](https://github.com/officialunofficial/polarize/releases/tag/v0.2.0) - 2026-08-21

### Bug Fixes

- key the app's actual focused window during raise-free activation,
  not always its main window
- embed a real bundle identity so Automation TCC can grant `polarize`
  its own permission
- resolve the raise-free activation window id from AX, not
  ScreenCaptureKit, closing an undisclosed Screen Recording dependency
- close review findings across the Tier 1, Tier 2, and Tier 3 tools,
  and the ones a live macOS session's own testing pass found
- hit-test system-wide, and share the AX write primitives
- give release-plz a git-only path for the two never-published crates

### Features

- expose all 25 tools: `screenshot`, `describe`, `tap`, `keyboard`,
  `perform_action`, `await_ui_element`, `await_screen_idle`,
  `run_applescript`, `script_dictionary`, `set_value`,
  `hit_test_at_point`, `set_window_frame`, `window_action`,
  `list_windows`, `app_launch`, `app_quit`, `list_displays`,
  `find_text`, `clipboard_read`/`clipboard_write`,
  `describe_notifications`, `dismiss_notification`, `frontmost_app`,
  `await_workspace_event`, and `record_flow`
- add tracing, structured per-tool-call logging to stderr
- resolve `SkyLight.framework`'s private symbols at runtime, then
  replace the private `SLEventPostToPid` with the public
  `CGEventPostToPid`
- give `tap` and `keyboard` a pid-scoped post path, reporting which
  native path each call actually took
- give `keyboard` a raise-free activation path that keys a target
  app's window without raising it or switching Space
- add a `request_automation_permission` tool, and disclaim the
  Automation bootstrap send's TCC responsibility

### Performance

- batch a node's AX attribute reads into one call, in both `describe`
  and the hit test

### Documentation

- record live-verified findings and corrections across
  `docs/INVARIANTS.md`'s background-input and Automation-permission
  invariants

## [0.1.0](https://github.com/officialunofficial/polarize/releases/tag/v0.1.0) - 2026-08-20

### Bug Fixes

- get polarize working e2e against a real running app
- address code-review findings (Standards + Spec)

### Features

- adopt cargo-dist and cargo-release for the release pipeline
- initial polarize MCP server (screenshot, describe, tap, keyboard)
