//! Icones do Phosphor (MIT), embutidos como fonte.
//!
//! O crate `egui-phosphor` faria isso sozinho, mas a versao publicada (0.13) depende do egui 0.35
//! e este app roda no 0.36. As duas versoes conviveriam no binario e o `FontDefinitions` de uma
//! nao serve para a outra, entao a fonte e vendorizada em `assets/` e so os codepoints usados
//! viram constante aqui. `assets/PHOSPHOR-LICENSE-MIT` guarda a licenca.

use eframe::egui;

pub const SPARKLE: &str = "\u{E6A2}";
pub const X: &str = "\u{E4F6}";
pub const GEAR: &str = "\u{E270}";
pub const TEXT_ALIGN_LEFT: &str = "\u{E484}";
pub const CARET_RIGHT: &str = "\u{E13A}";
pub const ARROW_CLOCKWISE: &str = "\u{E036}";
pub const COPY: &str = "\u{E1CA}";
pub const CHECK: &str = "\u{E182}";
pub const WARNING: &str = "\u{E4E0}";
pub const LIGHTNING: &str = "\u{E2DE}";

/// Entra logo depois da fonte de texto na familia proporcional, entao os icones resolvem antes
/// das fontes de emoji e o texto normal continua vindo da Ubuntu-Light.
pub fn install(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "phosphor".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/Phosphor.ttf"
        ))),
    );
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(1, "phosphor".to_owned());
    }
}
