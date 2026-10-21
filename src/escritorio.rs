//! Acciones de escritorio permitidas al modo comando. Lista cerrada: nada de shell.
//! Cada función devuelve un texto breve que el LLM usa para responder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

// ── aplicaciones ─────────────────────────────────────────────────────────────

struct App {
    ruta: PathBuf,
    nombre: String,
    /// nombre, nombre genérico y palabras clave normalizados, para buscar
    claves: Vec<String>,
}

fn normalizar(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            c => c,
        })
        .collect()
}

fn dirs_aplicaciones() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("applications"));
        v.push(d.join("flatpak/exports/share/applications"));
    }
    for d in ["/usr/share/applications", "/usr/local/share/applications", "/var/lib/flatpak/exports/share/applications", "/var/lib/snapd/desktop/applications"] {
        v.push(PathBuf::from(d));
    }
    v
}

fn leer_desktop(ruta: &Path) -> Option<App> {
    let txt = std::fs::read_to_string(ruta).ok()?;
    let (mut nombre, mut nombre_es, mut dominio) = (None, None, None);
    let mut claves = Vec::new();
    let mut en_entrada = false;
    for l in txt.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            en_entrada = l == "[Desktop Entry]";
            continue;
        }
        if !en_entrada {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        match k {
            "NoDisplay" | "Hidden" if v == "true" => return None,
            "Type" if v != "Application" => return None,
            "Name[es]" | "Name[es_MX]" if nombre_es.is_none() => nombre_es = Some(v.to_string()),
            "Name" => nombre = Some(v.to_string()),
            "X-GNOME-Gettext-Domain" | "X-Ubuntu-Gettext-Domain" => dominio = Some(v.to_string()),
            "GenericName" | "GenericName[es]" | "Keywords" | "Keywords[es]" | "X-GNOME-FullName[es]" => {
                claves.extend(v.split(';').filter(|s| !s.is_empty()).map(normalizar));
            }
            _ => {}
        }
    }
    // las apps de GNOME traducen el nombre con gettext en vez de traer Name[es]
    if nombre_es.is_none() {
        if let (Some(d), Some(n)) = (&dominio, &nombre) {
            nombre_es = traducir(d, n).filter(|t| t != n);
        }
    }
    if let Some(n) = &nombre {
        claves.push(normalizar(n));
    }
    let nombre = nombre_es.or(nombre)?;
    claves.insert(0, normalizar(&nombre));
    Some(App { ruta: ruta.to_path_buf(), nombre, claves })
}

fn traducir(dominio: &str, texto: &str) -> Option<String> {
    let out = Command::new("gettext").env("LANGUAGE", "es").args(["-d", dominio, texto]).output().ok()?;
    let t = String::from_utf8(out.stdout).ok()?;
    (!t.trim().is_empty()).then(|| t.trim().to_string())
}

/// Nombres comunes en español → palabras que aparecen en los .desktop.
const SINONIMOS: &[(&str, &[&str])] = &[
    ("navegador", &["browser", "web", "firefox", "chrome", "chromium"]),
    ("archivos", &["files", "nautilus", "file manager"]),
    ("explorador de archivos", &["files", "nautilus"]),
    ("editor de texto", &["text editor", "gedit", "editor"]),
    ("terminal", &["terminal", "console", "ptyxis"]),
    ("configuracion", &["settings", "ajustes"]),
    ("ajustes", &["settings", "configuracion"]),
    ("correo", &["mail", "thunderbird", "email"]),
    ("musica", &["music", "rhythmbox"]),
    ("fotos", &["photos", "image viewer", "loupe"]),
    ("imagenes", &["image viewer", "loupe", "photos"]),
    ("calendario", &["calendar"]),
    ("reloj", &["clocks"]),
    ("captura de pantalla", &["screenshot"]),
    ("hoja de calculo", &["calc", "spreadsheet"]),
    ("procesador de textos", &["writer", "word processor"]),
    ("excel", &["calc", "spreadsheet", "hoja de calculo"]),
    ("word", &["writer", "word processor"]),
    ("powerpoint", &["impress", "presentation"]),
];

/// Índice de aplicaciones, armado una vez por proceso (gettext es lento).
fn indice() -> &'static Vec<App> {
    static I: std::sync::OnceLock<Vec<App>> = std::sync::OnceLock::new();
    I.get_or_init(aplicaciones)
}

fn aplicaciones() -> Vec<App> {
    let mut v: Vec<App> = Vec::new();
    for d in dirs_aplicaciones() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "desktop") {
                if let Some(a) = leer_desktop(&p) {
                    // la primera ruta gana (las del usuario tapan a las del sistema)
                    if !v.iter().any(|x| x.ruta.file_name() == a.ruta.file_name()) {
                        v.push(a);
                    }
                }
            }
        }
    }
    v
}

pub fn abrir_aplicacion(nombre: &str) -> Result<String> {
    // «mi excel», «el navegador»: los posesivos y artículos no son parte del nombre;
    // «libreoffice-calc» es «libreoffice calc»
    let q = normalizar(&nombre.trim().replace(['-', '_'], " "));
    let q = ["mi ", "el ", "la ", "los ", "las ", "un ", "una "].iter().fold(q, |q, p| q.strip_prefix(p).map(str::to_string).unwrap_or(q));
    if q.is_empty() {
        bail!("falta el nombre de la aplicación");
    }
    let apps = indice();
    let sinonimos: Vec<&str> = SINONIMOS.iter().filter(|(k, _)| *k == q).flat_map(|(_, v)| v.iter().copied()).collect();
    // exacto en el nombre > nombre contiene > alguna clave contiene > sinónimo
    let puntaje = |a: &App| {
        if a.claves.iter().any(|c| *c == q) {
            4
        } else if a.claves[0].contains(&q) {
            3
        } else if a.claves.iter().any(|c| c.contains(&q) || (q.len() > 3 && q.contains(c.as_str()) && c.len() > 3)) {
            2
        } else if a.claves.iter().any(|c| sinonimos.iter().any(|s| c.contains(s))) {
            1
        } else {
            0
        }
    };
    let mejor = apps.iter().filter(|a| puntaje(a) > 0).max_by_key(|a| (puntaje(a), std::cmp::Reverse(a.nombre.len())));
    let Some(a) = mejor else {
        // se le dan al LLM los nombres más parecidos para que reintente
        let mut cerca: Vec<&App> = apps.iter().filter(|a| a.claves.iter().any(|c| c.chars().take(4).eq(q.chars().take(4)))).collect();
        cerca.truncate(8);
        let lista = cerca.iter().map(|a| a.nombre.as_str()).collect::<Vec<_>>().join(", ");
        bail!("no encontré una aplicación llamada «{nombre}». Parecidas: {}", if lista.is_empty() { "ninguna" } else { &lista });
    };
    lanzar("gio", &["launch".as_ref(), a.ruta.as_os_str()])?;
    Ok(format!("abrí {}", a.nombre))
}

/// Lanza un programa sin shell y sin esperar a que termine.
fn lanzar<S: AsRef<std::ffi::OsStr>>(prog: &str, args: &[S]) -> Result<()> {
    Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("no pude ejecutar {prog}"))?;
    Ok(())
}

// ── web y carpetas ───────────────────────────────────────────────────────────

pub fn abrir_url(url: &str) -> Result<String> {
    let url = url.trim();
    let url = if url.starts_with("http://") || url.starts_with("https://") { url.to_string() } else { format!("https://{url}") };
    if url.contains(char::is_whitespace) {
        bail!("URL inválida");
    }
    lanzar("gio", &["open", &url])?;
    Ok(format!("abrí {url}"))
}

fn codificar(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

pub fn buscar_web(consulta: &str) -> Result<String> {
    lanzar("gio", &["open", &format!("https://duckduckgo.com/?q={}", codificar(consulta))])?;
    Ok(format!("busqué «{consulta}» en la web"))
}

pub fn abrir_carpeta(nombre: &str) -> Result<String> {
    let n = normalizar(nombre.trim());
    let dir = match n.as_str() {
        "documentos" => dirs::document_dir(),
        "descargas" => dirs::download_dir(),
        "escritorio" => dirs::desktop_dir(),
        "imagenes" | "fotos" => dirs::picture_dir(),
        "musica" => dirs::audio_dir(),
        "videos" => dirs::video_dir(),
        "inicio" | "casa" | "carpeta personal" | "personal" => dirs::home_dir(),
        _ => {
            // ruta relativa dentro de la carpeta personal; nunca fuera de ella
            let home = dirs::home_dir().context("sin carpeta personal")?;
            let p = home.join(nombre.trim().trim_start_matches('~').trim_start_matches('/'));
            let p = p.canonicalize().with_context(|| format!("no existe la carpeta «{nombre}»"))?;
            if !p.starts_with(&home) || !p.is_dir() {
                bail!("solo puedo abrir carpetas dentro de tu carpeta personal");
            }
            Some(p)
        }
    }
    .context("no encontré esa carpeta")?;
    lanzar("gio", &["open".as_ref(), dir.as_os_str()])?;
    Ok(format!("abrí la carpeta {}", dir.display()))
}

// ── calculadora ──────────────────────────────────────────────────────────────

/// Evalúa aritmética. Los enteros se tratan como decimales para que 7/2 sea 3.5.
pub fn calcular(expresion: &str) -> Result<String> {
    // «23%» es porcentaje (23/100); «7 % 2» sigue siendo módulo
    let mut e1 = String::new();
    let cs: Vec<char> = expresion.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        let siguiente = cs[i + 1..].iter().find(|x| !x.is_whitespace());
        if c == '%' && !siguiente.is_some_and(|x| x.is_ascii_digit() || *x == '(') {
            e1.push_str("/100");
        } else {
            e1.push(c);
        }
    }
    let e = e1
        .replace('×', "*")
        .replace('÷', "/")
        .replace("raiz(", "math::sqrt(")
        .replace("sqrt(", "math::sqrt(")
        .replace(',', ".");
    let mut s = String::new();
    let cs: Vec<char> = e.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i].is_ascii_digit() && (i == 0 || !(cs[i - 1].is_alphanumeric() || cs[i - 1] == '.' || cs[i - 1] == '_')) {
            let ini = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            let num: String = cs[ini..i].iter().collect();
            s.push_str(&num);
            if !num.contains('.') {
                s.push_str(".0");
            }
            continue;
        }
        s.push(cs[i]);
        i += 1;
    }
    let v = evalexpr::eval(&s).map_err(|e| anyhow::anyhow!("no pude calcular «{expresion}»: {e}"))?;
    let n = v.as_number().map_err(|e| anyhow::anyhow!("{e}"))?;
    let texto = if (n - n.round()).abs() < 1e-9 && n.abs() < 1e15 { format!("{}", n.round() as i64) } else { format!("{:.6}", n).trim_end_matches('0').to_string() };
    Ok(texto)
}

// ── teclas ───────────────────────────────────────────────────────────────────

/// «ctrl+shift+t» → keysyms en orden de pulsación.
pub fn combinacion(c: &str) -> Result<Vec<u32>> {
    let mut v = Vec::new();
    for parte in c.split('+').map(|p| normalizar(p.trim())) {
        let ks = match parte.as_str() {
            "ctrl" | "control" => 0xffe3,
            "shift" | "mayus" | "mayusculas" => 0xffe1,
            "alt" => 0xffe9,
            "altgr" => 0xfe03,
            "super" | "windows" | "win" | "meta" => 0xffeb,
            "tab" | "tabulador" => 0xff09,
            "enter" | "intro" | "return" => 0xff0d,
            "esc" | "escape" => 0xff1b,
            "espacio" | "space" => 0x20,
            "borrar" | "backspace" | "retroceso" => 0xff08,
            "supr" | "suprimir" | "delete" => 0xffff,
            "inicio" | "home" => 0xff50,
            "fin" | "end" => 0xff57,
            "repag" | "pageup" | "re pag" => 0xff55,
            "avpag" | "pagedown" | "av pag" => 0xff56,
            "arriba" | "up" => 0xff52,
            "abajo" | "down" => 0xff54,
            "izquierda" | "left" => 0xff51,
            "derecha" | "right" => 0xff53,
            "imprpant" | "print" => 0xff61,
            p if p.starts_with('f') && p[1..].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n)) => 0xffbd + p[1..].parse::<u32>()?,
            p if p.chars().count() == 1 => p.chars().next().unwrap() as u32,
            p => bail!("tecla desconocida: «{p}»"),
        };
        v.push(ks);
    }
    // cerrar sesión o reiniciar la sesión gráfica no se permite
    let ctrl_alt = v.contains(&0xffe3) && v.contains(&0xffe9);
    if ctrl_alt && (v.contains(&0xffff) || v.contains(&0xff08)) {
        bail!("esa combinación no está permitida");
    }
    Ok(v)
}

// ── sistema ──────────────────────────────────────────────────────────────────

pub fn volumen(accion: &str, cantidad: Option<u32>) -> Result<String> {
    let paso = cantidad.unwrap_or(10).clamp(1, 50);
    let sink = "@DEFAULT_AUDIO_SINK@";
    let (args, msg): (Vec<String>, String) = match normalizar(accion).as_str() {
        "subir" => (vec!["set-volume".into(), "-l".into(), "1.0".into(), sink.into(), format!("{paso}%+")], format!("subí el volumen {paso}%")),
        "bajar" => (vec!["set-volume".into(), sink.into(), format!("{paso}%-")], format!("bajé el volumen {paso}%")),
        "silenciar" => (vec!["set-mute".into(), sink.into(), "1".into()], "silencié el audio".into()),
        "activar" => (vec!["set-mute".into(), sink.into(), "0".into()], "activé el audio".into()),
        a => bail!("acción de volumen desconocida: {a}"),
    };
    let ok = Command::new("wpctl").args(&args).status().context("no pude ejecutar wpctl")?.success();
    if !ok {
        bail!("wpctl falló");
    }
    Ok(msg)
}

/// ¿Hay un proceso con este nombre exacto? (p. ej. «iureditor» ya abierto)
pub fn corre(nombre: &str) -> bool {
    Command::new("pgrep").args(["-x", nombre]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn hora_fecha() -> String {
    use chrono::{Datelike, Timelike};
    let n = chrono::Local::now();
    const DIAS: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    const MESES: [&str; 12] = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
    format!(
        "{} {} de {} de {}, {}:{:02}",
        DIAS[n.weekday().num_days_from_monday() as usize],
        n.day(),
        MESES[n.month0() as usize],
        n.year(),
        n.hour(),
        n.minute()
    )
}

// ── lectura ──────────────────────────────────────────────────────────────────

/// Texto seleccionado (selección primaria); si no hay, el portapapeles.
pub fn seleccion() -> Result<String> {
    for args in [&["--primary", "--no-newline"][..], &["--no-newline"][..]] {
        let mut hijo = Command::new("wl-paste").args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("falta wl-paste")?;
        let t = std::time::Instant::now();
        // wl-paste puede quedarse esperando si nadie ofrece la selección
        while t.elapsed() < Duration::from_secs(2) {
            if hijo.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if hijo.try_wait()?.is_none() {
            let _ = hijo.kill();
            continue;
        }
        let mut s = String::new();
        use std::io::Read;
        hijo.stdout.take().map(|mut o| o.read_to_string(&mut s));
        if !s.trim().is_empty() {
            return Ok(s);
        }
    }
    bail!("no hay texto seleccionado ni en el portapapeles")
}

/// Descarga una página y extrae el texto legible (párrafos, títulos, listas).
pub fn texto_de_pagina(url: &str) -> Result<String> {
    let url = if url.starts_with("http") { url.to_string() } else { format!("https://{url}") };
    // una página de resultados no es un artículo: se leerían menús y enlaces
    let host = url.split('/').nth(2).unwrap_or("").to_lowercase();
    if ["duckduckgo.", "google.", "bing.", "search.yahoo.", "startpage.", "ecosia."].iter().any(|b| host.contains(b)) {
        bail!("esa es una página de resultados de búsqueda; no se lee. Responde tú brevemente o abre un resultado concreto");
    }
    let mut r = match ureq::get(&url).header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) iurevoz").header("Accept-Language", "es-MX,es;q=0.9").call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(c @ (401 | 403 | 429))) => bail!(
            "el sitio no deja leer su página automáticamente ({c}). Para encabezados usa su feed RSS si lo conoces \
             (p. ej. rss.nytimes.com/services/xml/rss/nyt/HomePage.xml) o propón otro medio"
        ),
        Err(e) => return Err(e.into()),
    };
    let html = r.body_mut().with_config().limit(4 << 20).read_to_string()?;
    // un feed RSS/Atom: los títulos son los encabezados
    let inicio = html.trim_start().get(..400).unwrap_or(html.trim_start()).to_lowercase();
    if inicio.contains("<rss") || inicio.contains("<feed") {
        let titulos = titulos_de_feed(&html);
        if !titulos.is_empty() {
            return Ok(titulos.join("\n"));
        }
    }
    let texto = extraer_texto(&html);
    if texto.trim().is_empty() {
        bail!("no encontré texto legible en la página");
    }
    Ok(texto)
}

/// Los `<title>` de un feed RSS/Atom (sin CDATA ni entidades básicas).
fn titulos_de_feed(xml: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut resto = xml;
    while let Some(i) = resto.find("<title") {
        let tras = &resto[i..];
        let Some(a) = tras.find('>') else { break };
        let Some(b) = tras.find("</title>") else { break };
        if a < b {
            let t = tras[a + 1..b].trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>").trim();
            let t = t.replace("&gt;", ">").replace("&lt;", "<").replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'");
            if !t.is_empty() {
                v.push(t);
            }
        }
        resto = &tras[b + 8..];
    }
    v.truncate(40);
    v
}

fn extraer_texto(html: &str) -> String {
    let bajo = html.to_lowercase();
    // quita bloques que nunca son contenido
    let mut limpio = String::with_capacity(html.len());
    let mut i = 0;
    'fuera: while i < html.len() {
        for etq in ["script", "style", "nav", "header", "footer", "aside", "noscript", "svg", "form"] {
            let abre = format!("<{etq}");
            if bajo[i..].starts_with(&abre) {
                let cierra = format!("</{etq}>");
                match bajo[i..].find(&cierra) {
                    Some(j) => i += j + cierra.len(),
                    None => i = html.len(),
                }
                continue 'fuera;
            }
        }
        let c = html[i..].chars().next().unwrap();
        limpio.push(c);
        i += c.len_utf8();
    }
    // texto dentro de párrafos, títulos y elementos de lista
    let bajo = limpio.to_lowercase();
    let mut partes = Vec::new();
    let mut i = 0;
    while let Some(j) = ["<p", "<h1", "<h2", "<h3", "<li"].iter().filter_map(|t| bajo[i..].find(t).map(|k| (k, *t))).min() {
        let (k, t) = j;
        let ini = i + k;
        let Some(fin_abre) = bajo[ini..].find('>') else { break };
        let nombre = &t[1..];
        let cierre = format!("</{nombre}");
        let cuerpo_ini = ini + fin_abre + 1;
        let fin = bajo[cuerpo_ini..].find(&cierre).map_or(bajo.len(), |x| cuerpo_ini + x);
        let t = quitar_etiquetas(&limpio[cuerpo_ini..fin]);
        if t.split_whitespace().count() >= 3 {
            partes.push(t);
        }
        i = fin.max(cuerpo_ini);
        if i >= bajo.len() {
            break;
        }
    }
    partes.join("\n")
}

fn quitar_etiquetas(s: &str) -> String {
    let mut out = String::new();
    let mut dentro = false;
    for c in s.chars() {
        match c {
            '<' => dentro = true,
            '>' => {
                dentro = false;
                out.push(' ');
            }
            c if !dentro => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&aacute;", "á")
        .replace("&eacute;", "é")
        .replace("&iacute;", "í")
        .replace("&oacute;", "ó")
        .replace("&uacute;", "ú")
        .replace("&ntilde;", "ñ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn calculadora() {
        assert_eq!(calcular("25 * 4 + 7").unwrap(), "107");
        assert_eq!(calcular("7 / 2").unwrap(), "3.5");
        assert_eq!(calcular("2^10").unwrap(), "1024");
        assert_eq!(calcular("sqrt(16)").unwrap(), "4");
        assert_eq!(calcular("1500 * 0,16").unwrap(), "240");
        assert_eq!(calcular("23%").unwrap(), "0.23");
        assert_eq!(calcular("1585 * 23%").unwrap(), "364.55");
        assert_eq!(calcular("7 % 2").unwrap(), "1");
    }

    #[test]
    fn teclas() {
        assert_eq!(combinacion("ctrl+t").unwrap(), vec![0xffe3, 0x74]);
        assert_eq!(combinacion("Alt+F4").unwrap(), vec![0xffe9, 0xffc1]);
        assert_eq!(combinacion("super").unwrap(), vec![0xffeb]);
        assert!(combinacion("ctrl+alt+supr").is_err());
    }

    #[test]
    fn extrae_parrafos() {
        let h = "<html><script>x()</script><nav><p>menu uno dos</p></nav><h1>El título del artículo</h1><p>Primer párrafo con <b>texto</b> &amp; más.</p></html>";
        assert_eq!(extraer_texto(h), "El título del artículo\nPrimer párrafo con texto & más.");
    }

    #[test]
    fn guiones_como_espacios() {
        // depende de LibreOffice instalado; si no está, no prueba nada
        if indice().iter().any(|a| a.nombre == "LibreOffice Calc") {
            let q = normalizar(&"libreoffice-calc".replace(['-', '_'], " "));
            assert!(indice().iter().any(|a| a.claves[0] == q));
        }
    }

    #[test]
    fn encuentra_calculadora() {
        // depende de las apps instaladas: solo comprueba que el índice se arma
        assert!(!indice().is_empty());
    }
}
