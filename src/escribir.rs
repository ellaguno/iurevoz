//! F4 · Escribir en Iurefficient: registrar horas, crear tareas y eventos, marcar avisos
//! como leídos. Todo pasa por la confirmación de voz/tarjeta, con un resumen armado en
//! código a partir de lo que de verdad se va a mandar (no del texto del modelo).

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
use reqwest::Method;
use serde_json::{Value, json};

use crate::iure::{self, Referencia};

pub const HERRAMIENTAS: &[&str] = &["registrar_horas", "crear_tarea", "crear_evento", "marcar_notificaciones_leidas"];

pub fn es_herramienta(nombre: &str) -> bool {
    HERRAMIENTAS.contains(&nombre)
}

/// Definiciones para el sub-agente de Iurefficient.
pub fn herramientas() -> Vec<Value> {
    let f = |nombre: &str, desc: &str, props: Value, req: &[&str]| {
        json!({"type": "function", "function": {"name": nombre, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": req}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let proyecto = s("nombre o id del proyecto; vacío = el último del que se habló");
    let fecha = s("fecha AAAA-MM-DD (calcúlala desde hoy; vacío = hoy)");
    vec![
        f(
            "registrar_horas",
            "Registra horas trabajadas en un proyecto (hoja de horas). Pide confirmación al usuario.",
            json!({"proyecto": proyecto, "horas": {"type": "number", "description": "horas, p. ej. 1.5"},
                   "descripcion": s("qué se hizo"), "fecha": fecha,
                   "facturable": {"type": "boolean", "description": "por omisión sí"}}),
            &["horas", "descripcion"],
        ),
        f(
            "crear_tarea",
            "Crea una tarea asignada al usuario, opcionalmente en un proyecto y con fecha límite. Pide confirmación.",
            json!({"titulo": s("título de la tarea"), "proyecto": s("nombre o id del proyecto (opcional)"),
                   "fecha_limite": s("fecha AAAA-MM-DD (opcional)"), "hora": s("hora HH:MM (opcional)"),
                   "descripcion": s("detalle (opcional)"),
                   "prioridad": {"type": "string", "enum": ["low", "medium", "high", "urgent"]}}),
            &["titulo"],
        ),
        f(
            "crear_evento",
            "Crea un evento en la agenda del usuario (reunión, audiencia, cita). Pide confirmación.",
            json!({"titulo": s("título"), "fecha": fecha, "hora": s("hora de inicio HH:MM; vacío = todo el día"),
                   "duracion_minutos": {"type": "integer", "description": "por omisión 60"},
                   "lugar": s("lugar (opcional)"), "proyecto": s("nombre o id del proyecto (opcional)")}),
            &["titulo", "fecha"],
        ),
        f(
            "marcar_notificaciones_leidas",
            "Marca como leídas notificaciones del usuario: las indicadas por id o todas. Pide confirmación.",
            json!({"ids": {"type": "array", "items": {"type": "string"}, "description": "ids de notificaciones"},
                   "todas": {"type": "boolean"}}),
            &[],
        ),
    ]
}

// ── fechas y cantidades para decir en voz alta ───────────────────────────────

fn fecha(a: &Value, campo: &str) -> Result<Option<NaiveDate>> {
    match a[campo].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(t) => NaiveDate::parse_from_str(&t[..t.len().min(10)], "%Y-%m-%d")
            .map(Some)
            .with_context(|| format!("«{campo}» debe ser AAAA-MM-DD, no «{t}»")),
    }
}

fn hora(a: &Value) -> Result<Option<NaiveTime>> {
    match a["hora"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(t) => NaiveTime::parse_from_str(t, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M:%S"))
            .map(Some)
            .with_context(|| format!("«hora» debe ser HH:MM, no «{t}»")),
    }
}

/// Hora local → ISO con zona (la instancia la convierte a UTC).
fn iso_local(d: NaiveDate, h: NaiveTime) -> Result<String> {
    let dt = Local
        .from_local_datetime(&d.and_time(h))
        .earliest()
        .context("esa hora no existe en tu zona horaria")?;
    Ok(dt.to_rfc3339())
}

pub fn dia_hablado(d: NaiveDate, hoy: NaiveDate) -> String {
    const DIAS: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    const MESES: [&str; 12] =
        ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
    let dif = (d - hoy).num_days();
    match dif {
        0 => "hoy".into(),
        1 => "mañana".into(),
        -1 => "ayer".into(),
        2..=6 => format!("el {} {}", DIAS[d.weekday().num_days_from_monday() as usize], d.day()),
        _ => format!("el {} de {}", d.day(), MESES[d.month0() as usize]),
    }
}

pub fn horas_habladas(h: f64) -> String {
    let enteras = h.trunc() as i64;
    let frac = h - h.trunc();
    if (frac - 0.5).abs() < 1e-6 {
        return match enteras {
            0 => "media hora".into(),
            1 => "hora y media".into(),
            n => format!("{n} horas y media"),
        };
    }
    if frac.abs() < 1e-6 {
        return if enteras == 1 { "una hora".into() } else { format!("{enteras} horas") };
    }
    format!("{} horas", format!("{h:.2}").trim_end_matches('0').replace('.', ","))
}

fn hora_hablada(h: NaiveTime) -> String {
    h.format("%-H:%M").to_string()
}

// ── proyectos ────────────────────────────────────────────────────────────────

fn parece_id(t: &str) -> bool {
    t.len() >= 32 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Nombre o id → (id, título), con la misma visibilidad que en la aplicación (vía MCP).
/// Vacío = el último proyecto del que se habló.
fn resolver_proyecto(texto: Option<&str>) -> Result<(String, String)> {
    let t = texto.map(str::trim).unwrap_or("");
    if t.is_empty() {
        if let Some(Referencia::Proyecto { id, titulo }) = iure::ultima() {
            return Ok((id, titulo));
        }
        bail!("¿en qué proyecto? Dime su nombre");
    }
    if parece_id(t) {
        let d = iure::consultar("detalle_proyecto", json!({"proyecto_id": t})).context("no encontré ese proyecto")?;
        return Ok((t.to_string(), d["titulo"].as_str().unwrap_or(t).to_string()));
    }
    let r = iure::consultar("listar_proyectos", json!({"busqueda": t, "limite": 6}))?;
    let ps = r["proyectos"].as_array().cloned().unwrap_or_default();
    let par = |p: &Value| (p["id"].as_str().unwrap_or("").to_string(), p["titulo"].as_str().unwrap_or("").to_string());
    match ps.len() {
        0 => bail!("no encontré un proyecto que se llame «{t}»"),
        1 => Ok(par(&ps[0])),
        _ => {
            let exacto: Vec<&Value> =
                ps.iter().filter(|p| p["titulo"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(t))).collect();
            if exacto.len() == 1 {
                return Ok(par(exacto[0]));
            }
            let nombres: Vec<&str> = ps.iter().take(4).filter_map(|p| p["titulo"].as_str()).collect();
            bail!("hay varios proyectos con «{t}»: {}. ¿Cuál?", nombres.join("; "))
        }
    }
}

// ── las acciones ─────────────────────────────────────────────────────────────

/// Ejecuta una escritura después de confirmarla. `confirmar` recibe el resumen hablado.
pub fn ejecutar(nombre: &str, a: &Value, confirmar: &dyn Fn(&str) -> bool) -> Result<String> {
    let hoy = Local::now().date_naive();
    let s = |k: &str| a[k].as_str().map(str::trim).filter(|x| !x.is_empty());
    match nombre {
        "registrar_horas" => {
            let horas = a["horas"].as_f64().filter(|h| *h > 0.0 && *h <= 24.0).context("las horas deben ser entre 0 y 24")?;
            let desc = s("descripcion").context("falta qué se hizo")?;
            let (pid, ptitulo) = resolver_proyecto(s("proyecto"))?;
            let dia = fecha(a, "fecha")?.unwrap_or(hoy);
            let facturable = a["facturable"].as_bool().unwrap_or(true);
            let resumen = format!(
                "Registro {} en {ptitulo}, {}, «{desc}»{}.",
                horas_habladas(horas),
                dia_hablado(dia, hoy),
                if facturable { "" } else { ", no facturable" }
            );
            if !confirmar(&resumen) {
                return Ok("el usuario no lo permitió".into());
            }
            let cuerpo = json!({"hours": horas, "description": desc, "billable": facturable,
                                "entry_date": dia.format("%Y-%m-%d").to_string()});
            iure::rest(Method::POST, &format!("/api/cases/{pid}/time-entries"), Some(cuerpo)).map_err(|e| {
                if e.to_string().contains("409") { anyhow::anyhow!("esa semana ya se envió y no admite horas nuevas") } else { e }
            })?;
            iure::recordar(Referencia::Proyecto { id: pid, titulo: ptitulo.clone() });
            Ok(format!("hecho: {} en {ptitulo}", horas_habladas(horas)))
        }
        "crear_tarea" => {
            let titulo = s("titulo").context("falta el título de la tarea")?;
            let proyecto = match s("proyecto") {
                Some(p) => Some(resolver_proyecto(Some(p))?),
                None => None,
            };
            let dia = fecha(a, "fecha_limite")?;
            let h = hora(a)?;
            let mut resumen = format!("Creo la tarea «{titulo}»");
            if let Some((_, pt)) = &proyecto {
                resumen += &format!(" en {pt}");
            }
            if let Some(d) = dia {
                resumen += &format!(", para {}", dia_hablado(d, hoy));
                if let Some(h) = h {
                    resumen += &format!(" a las {}", hora_hablada(h));
                }
            }
            resumen += ", asignada a ti.";
            if !confirmar(&resumen) {
                return Ok("el usuario no lo permitió".into());
            }
            let mut cuerpo = json!({"title": titulo, "assigned_to_id": iure::yo()?});
            if let Some((pid, _)) = &proyecto {
                cuerpo["case_id"] = json!(pid);
            }
            if let Some(d) = dia {
                // sin hora, al final del día: «vence hoy» no debe volverse «vencida» a mediodía
                let h = h.unwrap_or_else(|| NaiveTime::from_hms_opt(23, 59, 0).unwrap());
                cuerpo["due_date"] = json!(iso_local(d, h)?);
            }
            if let Some(d) = s("descripcion") {
                cuerpo["description"] = json!(d);
            }
            if let Some(p) = s("prioridad") {
                cuerpo["priority"] = json!(p);
            }
            let r = iure::rest(Method::POST, "/api/tasks", Some(cuerpo))?;
            let id = r["task"]["id"].as_str().unwrap_or("").to_string();
            if !id.is_empty() {
                iure::recordar(Referencia::Tarea { id, titulo: titulo.to_string() });
            }
            Ok(format!("hecho: tarea «{titulo}» creada"))
        }
        "crear_evento" => {
            let titulo = s("titulo").context("falta el título del evento")?;
            let dia = fecha(a, "fecha")?.unwrap_or(hoy);
            let h = hora(a)?;
            let minutos = a["duracion_minutos"].as_i64().filter(|m| (5..=24 * 60).contains(m)).unwrap_or(60);
            let proyecto = match s("proyecto") {
                Some(p) => Some(resolver_proyecto(Some(p))?),
                None => None,
            };
            let mut resumen = format!("Agendo «{titulo}» {}", dia_hablado(dia, hoy));
            match h {
                Some(h) => resumen += &format!(" a las {}, {} minutos", hora_hablada(h), minutos),
                None => resumen += ", todo el día",
            }
            if let Some(l) = s("lugar") {
                resumen += &format!(", en {l}");
            }
            if let Some((_, pt)) = &proyecto {
                resumen += &format!(", del proyecto {pt}");
            }
            resumen += ".";
            if !confirmar(&resumen) {
                return Ok("el usuario no lo permitió".into());
            }
            let (inicio, fin, todo) = match h {
                Some(h) => {
                    let i = Local.from_local_datetime(&dia.and_time(h)).earliest().context("esa hora no existe")?;
                    (i.to_rfc3339(), (i + chrono::Duration::minutes(minutos)).to_rfc3339(), false)
                }
                None => (
                    iso_local(dia, NaiveTime::MIN)?,
                    iso_local(dia, NaiveTime::from_hms_opt(23, 59, 0).unwrap())?,
                    true,
                ),
            };
            let mut cuerpo = json!({"title": titulo, "start_datetime": inicio, "end_datetime": fin, "all_day": todo});
            if let Some(l) = s("lugar") {
                cuerpo["location"] = json!(l);
            }
            if let Some((pid, _)) = &proyecto {
                cuerpo["case_id"] = json!(pid);
            }
            iure::rest(Method::POST, "/api/calendar-events", Some(cuerpo))?;
            Ok(format!("hecho: «{titulo}» quedó en tu agenda"))
        }
        "marcar_notificaciones_leidas" => {
            let ids: Vec<String> =
                a["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
            let todas = a["todas"].as_bool().unwrap_or(false) || ids.is_empty();
            let resumen = if todas {
                "Marco todas tus notificaciones como leídas.".to_string()
            } else if ids.len() == 1 {
                "Marco esa notificación como leída.".to_string()
            } else {
                format!("Marco esas {} notificaciones como leídas.", ids.len())
            };
            if !confirmar(&resumen) {
                return Ok("el usuario no lo permitió".into());
            }
            if todas {
                iure::rest(Method::PUT, "/api/notifications/mark-all-read", None)?;
                Ok("hecho: todas leídas".into())
            } else {
                for id in &ids {
                    iure::rest(Method::PUT, &format!("/api/notifications/{id}/read"), None)?;
                }
                Ok(format!("hecho: {} marcadas como leídas", ids.len()))
            }
        }
        _ => bail!("herramienta desconocida: {nombre}"),
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn horas_en_voz() {
        assert_eq!(horas_habladas(0.5), "media hora");
        assert_eq!(horas_habladas(1.0), "una hora");
        assert_eq!(horas_habladas(1.5), "hora y media");
        assert_eq!(horas_habladas(3.0), "3 horas");
        assert_eq!(horas_habladas(2.5), "2 horas y media");
        assert_eq!(horas_habladas(1.25), "1,25 horas");
    }

    #[test]
    fn dias_en_voz() {
        let hoy = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(); // martes
        let d = |m, dd| NaiveDate::from_ymd_opt(2026, m, dd).unwrap();
        assert_eq!(dia_hablado(d(10, 20), hoy), "hoy");
        assert_eq!(dia_hablado(d(10, 21), hoy), "mañana");
        assert_eq!(dia_hablado(d(10, 19), hoy), "ayer");
        assert_eq!(dia_hablado(d(10, 23), hoy), "el viernes 23");
        assert_eq!(dia_hablado(d(11, 3), hoy), "el 3 de noviembre");
    }

    #[test]
    fn validaciones_antes_de_confirmar() {
        let nunca = |_: &str| -> bool { panic!("no debía pedir confirmación") };
        assert!(ejecutar("registrar_horas", &json!({"horas": 30, "descripcion": "x"}), &nunca).is_err());
        assert!(ejecutar("crear_tarea", &json!({"titulo": " "}), &nunca).is_err());
        assert!(ejecutar("crear_evento", &json!({"titulo": "x", "fecha": "mañana"}), &nunca).is_err());
        assert!(ejecutar("crear_tarea", &json!({"titulo": "x", "hora": "10am", "fecha_limite": "2026-10-21"}), &nunca).is_err());
    }
}

#[cfg(test)]
mod con_instancia {
    /// Solo lectura contra la instancia real: `cargo test -- --ignored sesion_real`.
    #[test]
    #[ignore]
    fn sesion_real() {
        let id = crate::iure::yo().expect("sesión REST");
        assert!(!id.is_empty());
    }
}
