//! Traduce texto a la secuencia de keysyms que el teclado activo puede producir.
//!
//! GNOME descarta los keysyms que no están en la distribución de teclado activa. En
//! `latam` o `es`, por ejemplo, «á» no tiene tecla propia: se escribe con la tecla
//! muerta ´ seguida de «a». Aquí se lee la distribución de GNOME con libxkbcommon y,
//! para cada carácter sin tecla propia, se busca la combinación con tecla muerta.

use std::collections::HashSet;

use xkbcommon::xkb;

const RETURN: u32 = 0xff0d;
const TAB: u32 = 0xff09;

pub struct Teclas {
    /// Keysyms que alguna tecla produce directamente (con cualquier modificador).
    alcanzables: Option<HashSet<u32>>,
}

impl Teclas {
    /// Lee la primera distribución configurada en GNOME. Si falla, se asume que todo
    /// keysym es alcanzable (comportamiento sin traducción).
    pub fn de_gnome() -> Self {
        let (layout, variante) = distribucion_gnome().unwrap_or_else(|| ("us".into(), String::new()));
        Self::de_distribucion(&layout, &variante)
    }

    pub fn de_distribucion(layout: &str, variante: &str) -> Self {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(km) = xkb::Keymap::new_from_names(&ctx, "", "", layout, variante, None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        else {
            return Self { alcanzables: None };
        };
        let mut set = HashSet::new();
        for kc in km.min_keycode().raw()..=km.max_keycode().raw() {
            let kc = xkb::Keycode::new(kc);
            for nivel in 0..km.num_levels_for_key(kc, 0) {
                set.extend(km.key_get_syms_by_level(kc, 0, nivel).iter().map(|k| k.raw()));
            }
        }
        Self { alcanzables: Some(set) }
    }

    fn alcanzable(&self, ks: u32) -> bool {
        self.alcanzables.as_ref().is_none_or(|s| s.contains(&ks))
    }

    /// Keysyms a pulsar (y soltar) en orden para producir `c`.
    pub fn secuencia(&self, c: char) -> Vec<u32> {
        let directo = match c {
            '\n' => RETURN,
            '\t' => TAB,
            c => xkb::utf32_to_keysym(c as u32).raw(),
        };
        if self.alcanzable(directo) {
            return vec![directo];
        }
        if let Some((muerta, base)) = descomponer(c) {
            let b = xkb::utf32_to_keysym(base as u32).raw();
            if self.alcanzable(muerta) && self.alcanzable(b) {
                return vec![muerta, b];
            }
        }
        // sin alternativa: se intenta directo (puede perderse)
        vec![directo]
    }
}

/// Carácter acentuado → (tecla muerta, letra base).
fn descomponer(c: char) -> Option<(u32, char)> {
    const AGUDO: u32 = 0xfe51;
    const GRAVE: u32 = 0xfe50;
    const CIRCUNFLEJO: u32 = 0xfe52;
    const TILDE: u32 = 0xfe53;
    const DIERESIS: u32 = 0xfe57;
    let (m, b) = match c {
        'á' => (AGUDO, 'a'), 'é' => (AGUDO, 'e'), 'í' => (AGUDO, 'i'), 'ó' => (AGUDO, 'o'), 'ú' => (AGUDO, 'u'),
        'Á' => (AGUDO, 'A'), 'É' => (AGUDO, 'E'), 'Í' => (AGUDO, 'I'), 'Ó' => (AGUDO, 'O'), 'Ú' => (AGUDO, 'U'),
        'ý' => (AGUDO, 'y'), 'Ý' => (AGUDO, 'Y'),
        'à' => (GRAVE, 'a'), 'è' => (GRAVE, 'e'), 'ì' => (GRAVE, 'i'), 'ò' => (GRAVE, 'o'), 'ù' => (GRAVE, 'u'),
        'À' => (GRAVE, 'A'), 'È' => (GRAVE, 'E'), 'Ì' => (GRAVE, 'I'), 'Ò' => (GRAVE, 'O'), 'Ù' => (GRAVE, 'U'),
        'â' => (CIRCUNFLEJO, 'a'), 'ê' => (CIRCUNFLEJO, 'e'), 'î' => (CIRCUNFLEJO, 'i'), 'ô' => (CIRCUNFLEJO, 'o'), 'û' => (CIRCUNFLEJO, 'u'),
        'ä' => (DIERESIS, 'a'), 'ë' => (DIERESIS, 'e'), 'ï' => (DIERESIS, 'i'), 'ö' => (DIERESIS, 'o'), 'ü' => (DIERESIS, 'u'),
        'Ä' => (DIERESIS, 'A'), 'Ë' => (DIERESIS, 'E'), 'Ï' => (DIERESIS, 'I'), 'Ö' => (DIERESIS, 'O'), 'Ü' => (DIERESIS, 'U'),
        'ñ' => (TILDE, 'n'), 'Ñ' => (TILDE, 'N'), 'ã' => (TILDE, 'a'), 'õ' => (TILDE, 'o'),
        _ => return None,
    };
    Some((m, b))
}

/// Primera fuente de entrada de GNOME, p. ej. `[('xkb', 'latam')]` o `'us+intl'`.
fn distribucion_gnome() -> Option<(String, String)> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let i = s.find("('xkb', '")? + "('xkb', '".len();
    let fin = s[i..].find('\'')? + i;
    let nombre = &s[i..fin];
    let (l, v) = nombre.split_once('+').unwrap_or((nombre, ""));
    Some((l.to_string(), v.to_string()))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn latam_usa_tecla_muerta_para_acentos() {
        let t = Teclas::de_distribucion("latam", "");
        assert_eq!(t.secuencia('a'), vec![0x61]);
        assert_eq!(t.secuencia('ñ'), vec![0xf1]); // tecla propia en latam
        assert_eq!(t.secuencia('¿'), vec![0xbf]);
        assert_eq!(t.secuencia('á'), vec![0xfe51, 0x61]);
        assert_eq!(t.secuencia('É'), vec![0xfe51, 0x45]);
        assert_eq!(t.secuencia('\n'), vec![RETURN]);
    }

    #[test]
    fn us_intl_tambien() {
        let t = Teclas::de_distribucion("us", "intl");
        // en us+intl «á» sí tiene tecla con AltGr
        assert_eq!(t.secuencia('á'), vec![0xe1]);
    }
}
