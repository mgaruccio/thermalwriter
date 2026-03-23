use thermalwriter::theme::{ThemePalette, DefaultThemeSource, ManualThemeSource, ThemeSource};
use thermalwriter::config::Config;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn theme_palette_default_uses_neon_dash_colors() {
    let palette = ThemePalette::default();
    assert_eq!(palette.primary, "#e94560");
    assert_eq!(palette.secondary, "#53d8fb");
    assert_eq!(palette.accent, "#20f5d8");
    assert_eq!(palette.background, "#08080f");
    assert_eq!(palette.surface, "#12121e");
    assert_eq!(palette.text, "#e0e0e0");
    assert_eq!(palette.text_dim, "#888888");
    assert_eq!(palette.success, "#00ff88");
    assert_eq!(palette.warning, "#ffaa00");
    assert_eq!(palette.critical, "#ff3333");
}

#[test]
fn theme_palette_inject_into_context_adds_theme_prefix() {
    let palette = ThemePalette::default();
    let mut context = tera::Context::new();
    palette.inject_into_context(&mut context);

    let json = context.into_json();
    assert_eq!(json["theme_primary"].as_str().unwrap(), "#e94560");
    assert_eq!(json["theme_secondary"].as_str().unwrap(), "#53d8fb");
    assert_eq!(json["theme_accent"].as_str().unwrap(), "#20f5d8");
    assert_eq!(json["theme_background"].as_str().unwrap(), "#08080f");
    assert_eq!(json["theme_surface"].as_str().unwrap(), "#12121e");
    assert_eq!(json["theme_text"].as_str().unwrap(), "#e0e0e0");
    assert_eq!(json["theme_text_dim"].as_str().unwrap(), "#888888");
    assert_eq!(json["theme_success"].as_str().unwrap(), "#00ff88");
    assert_eq!(json["theme_warning"].as_str().unwrap(), "#ffaa00");
    assert_eq!(json["theme_critical"].as_str().unwrap(), "#ff3333");
}

#[test]
fn default_theme_source_loads_default_palette() {
    let source = DefaultThemeSource;
    let palette = source.load().unwrap();
    assert_eq!(palette.primary, "#e94560");
    assert_eq!(source.name(), "default");
}

#[test]
fn manual_theme_source_overrides_palette() {
    let mut custom = ThemePalette::default();
    custom.primary = "#aabbcc".to_string();
    let source = ManualThemeSource::new(custom);
    let palette = source.load().unwrap();
    assert_eq!(palette.primary, "#aabbcc");
    assert_eq!(source.name(), "manual");
}

#[test]
fn config_deserializes_without_theme_section() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 5
"#).unwrap();
    let cfg = Config::load(f.path()).unwrap();
    // ThemeConfig should use defaults when absent
    assert_eq!(cfg.theme.source, "");
    assert!(cfg.theme.manual.is_none());
    assert!(cfg.theme.background_image.is_none());
}

#[test]
fn config_deserializes_with_empty_theme_section() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, r#"
[display]
tick_rate = 5

[theme]
"#).unwrap();
    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.theme.source, "");
    assert!(cfg.theme.manual.is_none());
}

#[test]
fn config_deserializes_theme_with_manual_palette() {
    let mut f = NamedTempFile::new().unwrap();
    write!(f, "[theme]\nsource = \"manual\"\n\n[theme.manual]\nprimary = \"#aabbcc\"\nsecondary = \"#112233\"\naccent = \"#334455\"\nbackground = \"#000001\"\nsurface = \"#111112\"\ntext = \"#fefefe\"\ntext_dim = \"#ababab\"\nsuccess = \"#00fe00\"\nwarning = \"#fefe00\"\ncritical = \"#fe0000\"\n").unwrap();
    let cfg = Config::load(f.path()).unwrap();
    assert_eq!(cfg.theme.source, "manual");
    let manual = cfg.theme.manual.unwrap();
    assert_eq!(manual.primary, "#aabbcc");
    assert_eq!(manual.critical, "#fe0000");
}
