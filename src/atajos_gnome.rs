//! Atajos personalizados de GNOME (Configuración → Teclado) que ejecutan
//! `iurevoz alternar|comando|leer`. Es la alternativa cuando el portal
//! GlobalShortcuts no responde (pasa en GNOME 50 de Ubuntu).

use std::process::Command;

use anyhow::{Context, Result, bail};

const ESQUEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const BASE: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";

/// (id, nombre visible, orden de iurevoz, combinación)
pub const ATAJOS: &[(&str, &str, &str, &str)] = &[
    ("iurevoz-dictar", "IureVoz: dictar / detener", "alternar", "<Control><Alt>space"),
    ("iurevoz-comando", "IureVoz: dar una instrucción", "comando", "<Control><Alt>c"),
    ("iurevoz-leer", "IureVoz: leer selección / callar", "leer", "<Control><Alt>l"),
];

fn gsettings(args: &[&str]) -> Result<String> {
    let out = Command::new("gsettings").args(args).output().context("falta gsettings")?;
    if !out.status.success() {
        bail!("gsettings {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Rutas de los atajos personalizados actuales.
fn lista() -> Result<Vec<String>> {
    let s = gsettings(&["get", ESQUEMA, "custom-keybindings"])?;
    Ok(s.trim_start_matches("@as").trim().trim_matches(['[', ']']).split(',').map(|p| p.trim().trim_matches('\'').to_string()).filter(|p| !p.is_empty()).collect())
}

pub fn instalados() -> bool {
    lista().is_ok_and(|l| ATAJOS.iter().all(|(id, ..)| l.iter().any(|p| p.contains(id))))
}

/// Crea o actualiza los tres atajos sin tocar los demás del usuario.
pub fn instalar() -> Result<String> {
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let mut l = lista()?;
    for (id, nombre, orden, combinacion) in ATAJOS {
        let ruta = format!("{BASE}/{id}/");
        let esq = format!("{ESQUEMA}.custom-keybinding:{ruta}");
        gsettings(&["set", &esq, "name", nombre])?;
        gsettings(&["set", &esq, "command", &format!("'{exe}' {orden}")])?;
        gsettings(&["set", &esq, "binding", combinacion])?;
        if !l.contains(&ruta) {
            l.push(ruta);
        }
    }
    let valor = format!("[{}]", l.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(", "));
    gsettings(&["set", ESQUEMA, "custom-keybindings", &valor])?;
    Ok("Atajos instalados: Ctrl+Alt+Espacio dictar, Ctrl+Alt+C instrucción, Ctrl+Alt+L leer. Se cambian en Configuración → Teclado.".into())
}
