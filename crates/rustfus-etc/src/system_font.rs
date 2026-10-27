use std::sync::OnceLock;

use iced::Font;

pub fn configure_components() -> Font {
    let font = resolve();
    let family = match font.family {
        iced::font::Family::Name(name) => name,
        _ => "sans-serif",
    };
    shadcn_iced::font::set_sans(family);
    shadcn_iced::font::set_heading(None);
    shadcn_iced::font::set_mono(family);
    font
}

fn installed_fallback() -> Option<String> {
    use iced_graphics::text::{cosmic_text::fontdb, font_system};

    let mut system = font_system().write().ok()?;
    let db = system.raw().db();
    let query = fontdb::Query {
        families: &[fontdb::Family::SansSerif],
        ..Default::default()
    };
    let face = db
        .query(&query)
        .and_then(|id| db.face(id))
        .or_else(|| db.faces().find(|face| !face.monospaced))?;
    face.families.first().map(|(name, _)| name.clone())
}

/// Resolves the native text font once, falling back to iced's default on failure.
pub fn resolve() -> Font {
    static FONT: OnceLock<Font> = OnceLock::new();

    *FONT.get_or_init(|| {
        native_family()
            .or_else(installed_fallback)
            .filter(|name| !name.trim().is_empty())
            .map(|name| Font::with_name(Box::leak(name.into_boxed_str())))
            .unwrap_or(Font::DEFAULT)
    })
}

#[cfg(target_os = "macos")]
fn native_font() -> Option<core_text::font::CTFont> {
    use core_foundation::base::TCFType;
    use core_text::font::{CTFont, CTFontRef, CTFontUIFontType, kCTFontSystemFontType};

    unsafe extern "C" {
        fn CTFontCreateUIFontForLanguage(
            ui_type: CTFontUIFontType,
            size: f64,
            language: core_foundation::string::CFStringRef,
        ) -> CTFontRef;
    }

    // The safe new_ui_font_for_language wrapper panics on a null return.
    let font =
        unsafe { CTFontCreateUIFontForLanguage(kCTFontSystemFontType, 13.0, std::ptr::null()) };
    if font.is_null() {
        None
    } else {
        // Core Text transfers ownership of this newly created font to us.
        Some(unsafe { CTFont::wrap_under_create_rule(font) })
    }
}

#[cfg(target_os = "macos")]
fn native_family() -> Option<String> {
    use iced_graphics::text::{cosmic_text::fontdb, font_system};

    let native = native_font()?;
    let family = native.get_string_by_name_key(core_text::font::CTFontNameSpecifier::Family)?;
    let postscript =
        native.get_string_by_name_key(core_text::font::CTFontNameSpecifier::PostScript)?;
    let mut system = font_system().write().ok()?;

    let lookup = |db: &fontdb::Database| {
        // .AppleSystemUIFont is a Core Text alias, not necessarily an OpenType family.
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(&family)],
            ..Default::default()
        };
        if db.query(&query).is_some() {
            return Some(family.clone());
        }
        db.faces()
            .find(|face| face.post_script_name == postscript)
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.clone())
    };

    if let Some(name) = lookup(system.raw().db()) {
        return Some(name);
    }

    // Load only the OS-owned file behind the native font if fontdb missed it.
    let path = native.url()?.to_path()?;
    let bytes = std::fs::read(path).ok()?;
    system.load_font(std::borrow::Cow::Owned(bytes));
    lookup(system.raw().db())
}

#[cfg(target_os = "windows")]
fn native_family() -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SystemParametersInfoW,
    };

    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // The API writes a fully sized NONCLIENTMETRICSW to this valid buffer.
    let success = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
            0,
        )
    };
    if success == 0 {
        return None;
    }

    let name = &metrics.lfMessageFont.lfFaceName;
    let length = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    String::from_utf16(&name[..length]).ok()
}

#[cfg(target_os = "linux")]
fn native_family() -> Option<String> {
    let config = fontconfig::Fontconfig::new()?;
    Some(config.find("sans-serif", None)?.name)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn native_family() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadcn_text_families_use_the_resolved_system_font() {
        let font = configure_components();
        if let iced::font::Family::Name(family) = font.family {
            assert_eq!(
                shadcn_iced::font::sans().family,
                iced::font::Family::Name(family)
            );
            assert_eq!(
                shadcn_iced::font::mono().family,
                iced::font::Family::Name(family)
            );
            assert_eq!(
                shadcn_iced::font::heading(iced::font::Weight::Normal).family,
                iced::font::Family::Name(family)
            );
        }
    }

    #[test]
    fn resolution_is_stable_and_has_a_usable_family() {
        let font = resolve();
        assert_eq!(font, resolve());
        match font.family {
            iced::font::Family::Name(name) => assert!(!name.trim().is_empty()),
            _ => assert_eq!(font, Font::DEFAULT),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    fn native_query_matches_resolution() {
        match native_family().filter(|name| !name.trim().is_empty()) {
            Some(name) => match resolve().family {
                iced::font::Family::Name(resolved) => assert_eq!(resolved, name),
                other => panic!("expected native family {name}, got {other:?}"),
            },
            None => assert_eq!(resolve(), Font::DEFAULT),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolved_face_is_the_core_text_system_face() {
        use iced_graphics::text::{cosmic_text::fontdb, font_system};

        let Some(native) = native_font() else {
            assert_eq!(resolve(), Font::DEFAULT);
            return;
        };
        let font = resolve();
        let iced::font::Family::Name(family) = font.family else {
            panic!("the native system face could not be loaded into iced");
        };
        let mut system = font_system().write().expect("font system lock");
        let db = system.raw().db();
        let id = db
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..Default::default()
            })
            .expect("resolved family must be queryable by iced");
        assert_eq!(
            db.face(id).expect("resolved face").post_script_name,
            native.postscript_name(),
        );
    }
}
