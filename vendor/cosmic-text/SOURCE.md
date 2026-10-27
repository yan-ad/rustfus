# Source and local patch

Upstream: cosmic-text 0.15.0, https://crates.io/crates/cosmic-text/0.15.0
Repository: https://github.com/pop-os/cosmic-text
Packaged git revision: c82ee1c5b5b8032e91eaff1cb34294b538727a7d
License: MIT OR Apache-2.0 (both license texts are retained).

Copied from the cached crates.io package. Cargo.toml, Cargo.toml.orig, README.md, and unmodified source files retain their original contents. Cache-local .cargo-ok and .cargo_vcs_info.json are omitted. Upstream binary font fixtures and screenshots are omitted to avoid adding embedded font data and unused binary assets; their license notices remain in fonts/. Consequently, upstream integration rendering tests require restoring the upstream fixtures in a temporary test copy. No Apple font data is included.

The upstream LFS attributes are replaced with a vendor-only whitespace rule preserving upstream license/sample text; binary fixtures are not vendored. Runtime modifications are confined to src/font/mod.rs, src/shape.rs, and src/swash.rs: pin macOS .SFNS-Regular (System Font family) to optical size 17 for small UI text, and keep basic/advanced shaping, Swash images, and system-face outlines consistent. Dependencies, feature definitions, and the declared Rust 1.80 minimum are unchanged.

See ../../docs/SYSTEM_FONT_RENDERING.md for scope and limitations.
