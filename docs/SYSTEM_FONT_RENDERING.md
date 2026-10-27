# macOS system font optical sizing

The vendored `cosmic-text 0.15.0` patch applies a narrow macOS-only correction for the regular San Francisco system UI face (`.SFNS-Regular`, family beginning `System Font`). On that face only, the font location includes `opsz=17`, matching CoreText's small-text optical master used by the app's current 11 to 16 logical-pixel UI text. The existing `wght` coordinate remains active.

The optical-size decision is stored on `cosmic_text::Font`, so the same coordinate is available to the HarfRust shaping instance, basic-shaping glyph advances, Swash glyph images, and Swash outline generation. Non-macOS platforms, themed fonts, user fonts, and other San Francisco faces are unchanged. No Apple font data is copied, embedded, or modified.

This is intentionally a fixed small-text correction, not general size-aware optical sizing. It does not claim CoreText compositor pixel identity: CoreText and Swash still have different rasterization and hinting pipelines. It does ensure that shaping and rasterization use the same font variation coordinates.

`iced_tiny_skia` consumes cosmic-text's Swash images and therefore receives the correction through the vendored crate; no application glyph-spacing workaround is used.

## Verification

The vendored source retains upstream `Cargo.toml`, `Cargo.toml.orig`, `README.md`, `LICENSE-MIT`, and `LICENSE-APACHE` unchanged. `vendor/cosmic-text/SOURCE.md` records the source revision, patch surface, and exclusions: cache sentinels, binary font fixtures, and screenshots. There are no embedded font binaries. Upstream integration tests needing font fixtures were run from a temporary copy with the original upstream fixtures, not by adding them to the application.

Tests cover the platform and face-name predicate, normal/bold shaping coordinates against the native `/System/Library/Fonts/SFNS.ttf`, and the shared Swash settings (`wght` plus `opsz=17`). The standalone crate was checked with `std` without Swash and tested with `std,swash`. Cargo workspace exclusion and the path override belong in the root manifest, separate from this dependency patch.
