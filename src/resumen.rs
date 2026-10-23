//! F5 · «¿Qué tengo hoy?» y avisos de Iurefficient.
//!
//! El resumen se arma en código, no con el LLM: son tres o cuatro consultas fijas al
//! servidor MCP y una frase con plantilla, así que tarda lo que tarda la red y no inventa.
//! Los avisos sondean la instancia cada tantos minutos (no hay servidor nuevo ni push)
//! y anuncian cada cosa una sola vez.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate};
use serde_json::{Value, json};

use crate::config::Config;
use crate::iure;
use crate::voz::Voz;

/// «¿Qué tengo hoy?», «¿cómo viene mi día?», «resumen del día»… (se decide en código).
pub fn es_mi_dia(instruccion: &str) -> bool {
    let t: String = crate::correccion::palabras(instruccion).join(" ");
    [
        "qué tengo hoy",
        "que tengo hoy",
        "qué tengo para hoy",
        "que tengo para hoy",
        "mi día",
        "mi dia",
        "resumen del día",
        "resumen del dia",
        "resumen de hoy",
        "qué hay para hoy",
        "que hay para hoy",
        "pendientes de hoy",
    ]
    .iter()
    .any(|f| t.contains(f))
}

fn texto(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

/// Fecha ISO de la instancia (UTC) en hora local.
fn local(iso: &Value) -> Option<DateTime<Local>> {
    let s = iso.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Local))
        .or_else(|| {
            // sin zona: la instancia guarda UTC
            chrono::NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc().with_timezone(&Local))
        })
}

fn hora(d: &DateTime<Local>) -> String {
    d.format("%-H:%M").to_string()
}

/// «la audiencia, el amparo y 3 más».
fn enumerar(nombres: &[String], total: usize) -> String {
    let n: Vec<&String> = nombres.iter().filter(|s| !s.is_empty()).take(3).collect();
    let resto = total.saturating_sub(n.len());
    let mut partes: Vec<String> = n.iter().map(|s| s.to_string()).collect();
    if resto > 0 {
        partes.push(if resto == 1 { "otra más".into() } else { format!("{resto} más") });
    }
    match partes.len() {
        0 => String::new(),
        1 => partes.remove(0),
        _ => {
            let ultimo = partes.pop().unwrap();
            format!("{} y {ultimo}", partes.join(", "))
        }
    }
}

fn plural(n: usize, uno: &str, varios: &str) -> String {
    if n == 1 { format!("una {uno}") } else { format!("{n} {varios}") }
}

/// Tareas de una lista separadas en atrasadas (antes de hoy) y de hoy.
fn por_dia(tareas: &[Value], hoy: NaiveDate) -> (Vec<&Value>, Vec<&Value>) {
    let mut atrasadas = vec![];
    let mut de_hoy = vec![];
    for t in tareas {
        match local(&t["fecha_limite"]).map(|d| d.date_naive()) {
            Some(d) if d < hoy => atrasadas.push(t),
            Some(d) if d == hoy => de_hoy.push(t),
            _ => {}
        }
    }
    (atrasadas, de_hoy)
}

fn titulos(ts: &[&Value]) -> Vec<String> {
    ts.iter().map(|t| texto(&t["titulo"])).collect()
}

/// El resumen hablado del día de quien usa IureVoz.
pub fn mi_dia() -> Result<String> {
    let hoy = Local::now().date_naive();
    let mut frases = vec![];

    if iure::ofrece("mis_tareas") && iure::ofrece("agenda") {
        let ag = iure::consultar("agenda", json!({}))?;
        let eventos = ag["eventos"].as_array().cloned().unwrap_or_default();
        if !eventos.is_empty() {
            let desc: Vec<String> = eventos
                .iter()
                .map(|e| {
                    let t = texto(&e["titulo"]);
                    match (e["todo_el_dia"].as_bool(), local(&e["inicio"])) {
                        (Some(true), _) | (_, None) => t,
                        (_, Some(d)) if d.date_naive() < hoy => format!("{t}, que sigue desde antes"),
                        (_, Some(d)) => format!("{t} a las {}", hora(&d)),
                    }
                })
                .collect();
            let total = ag["total_eventos"].as_u64().map_or(eventos.len(), |n| n as usize);
            frases.push(format!("En la agenda tienes {}: {}.", plural(total, "cita", "citas"), enumerar(&desc, total)));
        }

        let todas = iure::consultar("mis_tareas", json!({"cuando": "vencidas", "limite": 50}))?;
        let de_hoy = iure::consultar("mis_tareas", json!({"cuando": "hoy", "limite": 50}))?;
        let hoy_l = de_hoy["tareas"].as_array().cloned().unwrap_or_default();
        let hoy_refs: Vec<&Value> = hoy_l.iter().collect();
        if !hoy_refs.is_empty() {
            let n = de_hoy["total"].as_u64().map_or(hoy_refs.len(), |n| n as usize);
            frases.push(format!(
                "{} para hoy: {}.",
                if n == 1 { "Vence una tarea".to_string() } else { format!("Vencen {n} tareas") },
                enumerar(&titulos(&hoy_refs), n)
            ));
        }
        let venc = todas["tareas"].as_array().cloned().unwrap_or_default();
        let (atrasadas, _) = por_dia(&venc, hoy);
        if !atrasadas.is_empty() {
            // `total` incluye las de hoy cuya hora ya pasó; esas ya se dijeron arriba
            let de_hoy_ya = venc.len() - atrasadas.len();
            let n = (todas["total"].as_u64().unwrap_or(0) as usize).saturating_sub(de_hoy_ya).max(atrasadas.len());
            frases.push(format!(
                "Tienes {} atrasadas; la más antigua es {}.",
                plural(n, "tarea", "tareas"),
                texto(&atrasadas[0]["titulo"])
            ));
        }
        if iure::ofrece("notificaciones") {
            let nt = iure::consultar("notificaciones", json!({"limite": 1}))?;
            let n = nt["no_leidas"].as_u64().unwrap_or(0) as usize;
            if n > 0 {
                frases.push(format!("Y {} sin leer.", plural(n, "notificación", "notificaciones")));
            }
        }
        if frases.is_empty() {
            frases.push("Hoy no tienes citas ni entregas, y nada atrasado.".into());
        }
    } else {
        // instancia sin las herramientas de «mi día»: lo que vence en los proyectos que ves
        let v = iure::consultar("proximos_vencimientos", json!({"dias": 1, "limite": 50}))?;
        let lista = v["vencimientos"].as_array().cloned().unwrap_or_default();
        let (atrasadas, de_hoy) = por_dia(&lista, hoy);
        if !de_hoy.is_empty() {
            frases.push(format!(
                "En tus proyectos {} hoy: {}.",
                if de_hoy.len() == 1 { "vence una tarea".to_string() } else { format!("vencen {} tareas", de_hoy.len()) },
                enumerar(&titulos(&de_hoy), de_hoy.len())
            ));
        }
        if !atrasadas.is_empty() {
            let n = (v["total"].as_u64().unwrap_or(0) as usize).saturating_sub(de_hoy.len()).max(atrasadas.len());
            frases.push(format!(
                "En los proyectos que ves hay {} atrasadas, no solo tuyas; la más antigua es {}.",
                plural(n, "tarea", "tareas"),
                texto(&atrasadas[0]["titulo"])
            ));
        }
        if frases.is_empty() {
            frases.push("En tus proyectos no vence nada hoy ni hay nada atrasado.".into());
        }
        frases.push("Cuando tu instancia se actualice te diré solo lo tuyo, con tus citas.".into());
    }
    Ok(frases.join(" "))
}

// ── avisos ───────────────────────────────────────────────────────────────────

fn ruta_vistos() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("iurevoz").join("avisos-vistos.json")
}

fn leer_vistos() -> Option<BTreeSet<String>> {
    let s = std::fs::read_to_string(ruta_vistos()).ok()?;
    serde_json::from_str(&s).ok()
}

fn guardar_vistos(v: &BTreeSet<String>) {
    let r = ruta_vistos();
    if let Some(d) = r.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(r, serde_json::to_string(v).unwrap_or_default());
}

/// Lo que hay que avisar ahora: (clave única, texto). Necesita las herramientas de «mi día»:
/// sin ellas solo habría los vencimientos de TODOS los proyectos visibles, demasiado ruido.
fn pendientes_de_aviso() -> Result<Vec<(String, String)>> {
    let mut salida = vec![];
    if !iure::ofrece("mis_tareas") {
        return Ok(salida);
    }
    let hoy = Local::now().date_naive();
    for cuando in ["hoy", "vencidas"] {
        let r = iure::consultar("mis_tareas", json!({"cuando": cuando, "limite": 50}))?;
        for t in r["tareas"].as_array().cloned().unwrap_or_default() {
            let id = texto(&t["id"]);
            let titulo = texto(&t["titulo"]);
            let fecha = local(&t["fecha_limite"]);
            let vencida = t["vencida"].as_bool().unwrap_or(false);
            let (clave, txt) = if vencida {
                (format!("vencida:{id}:{}", texto(&t["fecha_limite"])), format!("Se venció la tarea {titulo}"))
            } else {
                let h = fecha.filter(|d| d.date_naive() == hoy).map(|d| format!(" a las {}", hora(&d))).unwrap_or_default();
                (format!("hoy:{id}:{}", texto(&t["fecha_limite"])), format!("Hoy vence {titulo}{h}"))
            };
            salida.push((clave, txt));
        }
    }
    if iure::ofrece("notificaciones") {
        let r = iure::consultar("notificaciones", json!({"limite": 20}))?;
        for n in r["notificaciones"].as_array().cloned().unwrap_or_default() {
            salida.push((format!("notif:{}", texto(&n["id"])), texto(&n["titulo"])));
        }
    }
    Ok(salida)
}

/// Una pasada: avisa lo nuevo y lo marca como visto. Devuelve el texto avisado.
fn una_pasada(cfg: &Config, voz: Option<&Voz>, ocupado: &dyn Fn() -> bool) -> Result<Option<String>> {
    let actuales = pendientes_de_aviso()?;
    let primera_vez = leer_vistos().is_none();
    let vistos = leer_vistos().unwrap_or_default();
    let nuevos: Vec<&(String, String)> = actuales.iter().filter(|(k, _)| !vistos.contains(k)).collect();
    // se recuerdan solo las claves vigentes, para que el archivo no crezca sin fin
    let vigentes: BTreeSet<String> = actuales.iter().map(|(k, _)| k.clone()).collect();
    if primera_vez || nuevos.is_empty() {
        // la primera vez no se anuncia lo acumulado: para eso está «¿qué tengo hoy?»
        guardar_vistos(&vigentes);
        return Ok(None);
    }
    let textos: Vec<String> = nuevos.iter().map(|(_, t)| t.clone()).collect();
    let cuerpo = if textos.len() == 1 {
        format!("{}.", textos[0])
    } else {
        format!("{}.", enumerar(&textos, textos.len()))
    };
    notificar("Iurefficient", &cuerpo);
    if cfg.iure.avisos_hablados && !ocupado() {
        if let Some(v) = voz {
            v.decir(cuerpo.clone());
        }
    }
    guardar_vistos(&vigentes);
    Ok(Some(cuerpo))
}

fn notificar(titulo: &str, cuerpo: &str) {
    crate::plataforma::notificar(titulo, cuerpo, "appointment-soon");
}

fn conectado() -> bool {
    matches!(iure::estado(), iure::Estado::Conectado(_))
}

/// Al arrancar la ventana: resumen hablado (si se pidió) y sondeo de avisos.
/// La configuración se relee en cada vuelta, así que los cambios en Ajustes valen sin reiniciar.
pub fn arrancar(voz: Option<Voz>, ocupado: impl Fn() -> bool + Send + 'static) {
    std::thread::Builder::new()
        .name("avisos".into())
        .spawn(move || {
            // que primero cargue lo demás (modelos, LLM); la red puede esperar
            std::thread::sleep(Duration::from_secs(20));
            let cfg = Config::cargar();
            if cfg.iure.resumen_al_iniciar && conectado() {
                match mi_dia() {
                    Ok(t) => {
                        notificar("Tu día", &t);
                        if let Some(v) = &voz {
                            v.decir(t);
                        }
                    }
                    Err(e) => eprintln!("resumen del día: {e:#}"),
                }
            }
            loop {
                let cfg = Config::cargar();
                if cfg.iure.avisos && conectado() {
                    match una_pasada(&cfg, voz.as_ref(), &ocupado) {
                        Ok(Some(t)) => eprintln!("aviso de Iurefficient: {t}"),
                        Ok(None) => {}
                        Err(e) => eprintln!("avisos de Iurefficient: {e:#}"),
                    }
                }
                std::thread::sleep(Duration::from_secs(60 * cfg.iure.cada_minutos.max(5) as u64));
            }
        })
        .expect("hilo avisos");
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn frases_de_mi_dia() {
        assert!(es_mi_dia("¿Qué tengo hoy?"));
        assert!(es_mi_dia("oye, ¿cómo viene mi día?"));
        assert!(es_mi_dia("dame el resumen del día"));
        assert!(!es_mi_dia("¿qué tengo en Descargas?"));
        assert!(!es_mi_dia("¿qué tareas tengo en Heralva?"));
    }

    #[test]
    fn enumeraciones() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(enumerar(&n(&["a"]), 1), "a");
        assert_eq!(enumerar(&n(&["a", "b"]), 2), "a y b");
        assert_eq!(enumerar(&n(&["a", "b", "c", "d"]), 4), "a, b, c y otra más");
        assert_eq!(enumerar(&n(&["a", "b", "c"]), 7), "a, b, c y 4 más");
    }

    #[test]
    fn fechas_de_la_instancia() {
        let d = local(&json!("2026-10-20T18:30:00Z")).unwrap();
        assert_eq!(d.with_timezone(&chrono::Utc).format("%H:%M").to_string(), "18:30");
        assert!(local(&json!("2026-10-20T18:30:00.123456")).is_some());
        assert!(local(&Value::Null).is_none());
    }
}
