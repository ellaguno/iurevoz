//! Normalización de texto para la voz: cifras, montos, fechas, horas, teléfonos,
//! abreviaturas y siglas pasan a palabras antes del TTS («$1,160.00» → «mil ciento
//! sesenta pesos», «art. 14» → «artículo catorce»). Español de México: coma de miles,
//! punto decimal.

/// Normaliza línea por línea (los saltos de línea separan oraciones y se conservan).
pub fn normalizar(texto: &str) -> String {
    let mayusculas = {
        let letras: Vec<char> = texto.chars().filter(|c| c.is_alphabetic()).collect();
        letras.len() > 30 && letras.iter().filter(|c| c.is_uppercase()).count() * 10 > letras.len() * 6
    };
    texto.lines().map(|l| linea(&telefonos(l), mayusculas)).collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------- números

const UNIDADES: [&str; 30] = [
    "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve", "diez", "once", "doce", "trece",
    "catorce", "quince", "dieciséis", "diecisiete", "dieciocho", "diecinueve", "veinte", "veintiuno", "veintidós",
    "veintitrés", "veinticuatro", "veinticinco", "veintiséis", "veintisiete", "veintiocho", "veintinueve",
];
const DECENAS: [&str; 10] = ["", "", "", "treinta", "cuarenta", "cincuenta", "sesenta", "setenta", "ochenta", "noventa"];
const CENTENAS: [&str; 10] =
    ["", "ciento", "doscient", "trescient", "cuatrocient", "quinient", "seiscient", "setecient", "ochocient", "novecient"];

/// Cómo termina un «uno»: «uno» suelto, «un» ante sustantivo masculino, «una» ante femenino.
#[derive(Clone, Copy, PartialEq)]
enum Genero {
    Suelto,
    Masc,
    Fem,
}

fn menor_mil(n: u64, g: Genero) -> String {
    let (c, r) = (n / 100, n % 100);
    let mut p = Vec::new();
    if c > 0 {
        p.push(match c {
            1 if r == 0 => "cien".to_string(),
            1 => "ciento".to_string(),
            _ => format!("{}{}", CENTENAS[c as usize], if g == Genero::Fem { "as" } else { "os" }),
        });
    }
    if r > 0 {
        let mut s = if r < 30 { UNIDADES[r as usize].to_string() } else {
            let (d, u) = (r / 10, r % 10);
            if u == 0 { DECENAS[d as usize].to_string() } else { format!("{} y {}", DECENAS[d as usize], UNIDADES[u as usize]) }
        };
        if s.ends_with("uno") {
            match g {
                Genero::Masc => s.truncate(s.len() - 1),
                Genero::Fem => s = format!("{}a", &s[..s.len() - 1]),
                Genero::Suelto => {}
            }
            if s == "veintiun" {
                s = "veintiún".into();
            }
        }
        p.push(s);
    }
    p.join(" ")
}

fn cardinal(n: u64, g: Genero) -> String {
    if n == 0 {
        return "cero".into();
    }
    if n >= 1_000_000_000_000 {
        return digitos(&n.to_string());
    }
    let (millones, resto) = (n / 1_000_000, n % 1_000_000);
    let (miles, unidades) = (resto / 1000, resto % 1000);
    let mut p = Vec::new();
    if millones > 0 {
        p.push(if millones == 1 { "un millón".to_string() } else { format!("{} millones", cardinal(millones, Genero::Masc)) });
    }
    if miles > 0 {
        let g_miles = if g == Genero::Fem { Genero::Fem } else { Genero::Masc };
        p.push(if miles == 1 { "mil".to_string() } else { format!("{} mil", menor_mil(miles, g_miles)) });
    }
    if unidades > 0 {
        p.push(menor_mil(unidades, g));
    }
    p.join(" ")
}

fn digitos(s: &str) -> String {
    s.chars().filter_map(|c| c.to_digit(10)).map(|d| UNIDADES[d as usize]).collect::<Vec<_>>().join(" ")
}

/// Parte decimal: «5» → cinco, «75» → setenta y cinco, «05» → cero cinco.
fn decimales(s: &str) -> String {
    if s.starts_with('0') || s.len() > 2 { digitos(s) } else { cardinal(s.parse().unwrap_or(0), Genero::Suelto) }
}

fn ordinal(n: u64, fem: bool, apocope: bool) -> String {
    const O: [&str; 11] = ["", "primer", "segund", "tercer", "cuart", "quint", "sext", "séptim", "octav", "noven", "décim"];
    let base = |k: usize| -> String {
        if apocope && !fem && (k == 1 || k == 3) {
            O[k].to_string()
        } else {
            format!("{}{}", O[k], if fem { "a" } else { "o" })
        }
    };
    match n {
        1..=10 => base(n as usize),
        11..=19 => format!("{} {}", if fem { "décima" } else { "décimo" }, base(n as usize - 10)),
        20 => (if fem { "vigésima" } else { "vigésimo" }).into(),
        _ => cardinal(n, if fem { Genero::Fem } else { Genero::Suelto }),
    }
}

struct Numero {
    entero: u64,
    dec: Option<String>,
}

/// «1,160.00», «12,500», «1160», «3.5», «0.75». Una coma seguida de 1–2 cifras es decimal.
fn numero(s: &str) -> Option<Numero> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') || !s.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if s.matches('.').count() > 1 || s.ends_with([',', '.']) {
        return None;
    }
    let (ent, dec) = match s.split_once('.') {
        Some((e, d)) => (e, Some(d.to_string())),
        None => match s.rsplit_once(',') {
            Some((e, d)) if d.len() != 3 && !e.contains(',') => (e, Some(d.to_string())),
            _ => (s, None),
        },
    };
    if ent.contains(',') {
        let grupos: Vec<&str> = ent.split(',').collect();
        if grupos[0].is_empty() || grupos[0].len() > 3 || grupos[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
    }
    Some(Numero { entero: ent.replace(',', "").parse().ok()?, dec })
}

// ---------------------------------------------------------------- vocabulario

const MESES: [&str; 12] =
    ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];

const FEMENINOS: &[&str] = &[
    "hora", "semana", "página", "foja", "hoja", "copia", "persona", "audiencia", "cláusula", "sala", "cuota", "parte", "vez",
    "veces", "demanda", "sentencia", "prueba", "cuenta", "factura", "nota", "cita", "llamada", "tarea", "carpeta", "empresa",
    "casa", "mujer", "mujeres", "pieza", "foto", "fotografía", "noche", "tarde", "mañana", "fecha", "firma", "acta",
    "instancia", "etapa", "pregunta", "respuesta", "palabra", "línea", "fila", "columna", "imagen", "imágenes", "quincena",
    "tonelada", "libra", "milla", "letra", "caja", "vía", "fracción", "sesión", "junta", "materia", "multa", "pensión",
];

fn es_femenino(palabra: &str) -> bool {
    let p = palabra.to_lowercase();
    let singular = p.strip_suffix('s').unwrap_or(&p);
    FEMENINOS.contains(&p.as_str())
        || FEMENINOS.contains(&singular)
        || ["ción", "ciones", "sión", "siones", "dad", "dades", "tad", "tades", "tud", "tudes", "umbre", "umbres"]
            .iter()
            .any(|s| p.ends_with(s))
}

/// Palabras tras las que un número no va pegado a un sustantivo («el 1 y el 2»).
const NO_SUSTANTIVO: &[&str] = &[
    "y", "o", "u", "e", "de", "del", "a", "al", "en", "por", "para", "con", "que", "más", "menos", "entre", "sin", "sobre",
    "es", "son", "fue", "era", "mil", "millones", "millón",
];

fn abreviatura(a: &str, anterior: &str) -> Option<&'static str> {
    Some(match a {
        "art" => "artículo",
        "arts" => "artículos",
        "fr" | "frac" | "fracc" => "fracción",
        "inc" => "inciso",
        "núm" | "num" => "número",
        "núms" => "números",
        "lic" | "licdo" | "lcdo" if anterior == "la" => "licenciada",
        "lic" | "licdo" | "lcdo" => "licenciado",
        "lics" => "licenciados",
        "licda" | "lcda" => "licenciada",
        "dr" if anterior == "la" => "doctora",
        "dr" => "doctor",
        "dra" => "doctora",
        "sr" => "señor",
        "sra" => "señora",
        "srta" => "señorita",
        "ing" => "ingeniero",
        "mtro" => "maestro",
        "mtra" => "maestra",
        "arq" => "arquitecto",
        "pág" | "pag" => "página",
        "págs" | "pags" => "páginas",
        "aprox" => "aproximadamente",
        "tel" => "teléfono",
        "cel" => "celular",
        "exp" => "expediente",
        "av" => "avenida",
        "col" => "colonia",
        "cía" => "compañía",
        "depto" => "departamento",
        "admón" => "administración",
        "gral" => "general",
        "vs" => "contra",
        "máx" => "máximo",
        "mín" => "mínimo",
        "ud" => "usted",
        "uds" => "ustedes",
        "cap" => "capítulo",
        "tít" => "título",
        "párr" => "párrafo",
        "fs" => "fojas",
        "hrs" | "hr" => "horas",
        "min" => "minutos",
        "seg" => "segundos",
        _ => return None,
    })
}

/// Unidades tras una cifra: (singular, plural).
fn unidad(u: &str) -> Option<(&'static str, &'static str)> {
    Some(match u {
        "km" => ("kilómetro", "kilómetros"),
        "kg" => ("kilo", "kilos"),
        "g" | "gr" => ("gramo", "gramos"),
        "cm" => ("centímetro", "centímetros"),
        "mm" => ("milímetro", "milímetros"),
        "m" | "mts" => ("metro", "metros"),
        "m2" | "m²" => ("metro cuadrado", "metros cuadrados"),
        "ml" => ("mililitro", "mililitros"),
        "l" | "lt" | "lts" => ("litro", "litros"),
        "gb" => ("giga", "gigas"),
        "mb" => ("mega", "megas"),
        "h" | "hr" | "hrs" => ("hora", "horas"),
        "min" => ("minuto", "minutos"),
        "seg" | "s" => ("segundo", "segundos"),
        "°" | "°c" | "º" => ("grado", "grados"),
        "kw" => ("kilowatt", "kilowatts"),
        "kwh" => ("kilowatt hora", "kilowatts hora"),
        _ => return None,
    })
}

/// Siglas con su lectura habitual en México; las desconocidas se deletrean o se leen como palabra.
fn sigla(s: &str) -> Option<&'static str> {
    Some(match s {
        "IVA" => "iva",
        "ISR" => "i ese erre",
        "IEPS" => "i e pe ese",
        "SAT" => "sat",
        "IMSS" => "i eme ese ese",
        "ISSSTE" => "isste",
        "INFONAVIT" => "infonavit",
        "FOVISSSTE" => "fovisste",
        "CURP" => "curp",
        "RFC" => "erre efe ce",
        "CFF" => "ce efe efe",
        "LFT" => "ele efe te",
        "LFDA" => "ele efe de a",
        "CNPP" => "ce ene pe pe",
        "CNPCF" => "ce ene pe ce efe",
        "SCJN" => "ese ce jota ene",
        "TFJA" => "te efe jota a",
        "CDMX" => "ce de eme equis",
        "UNAM" => "unam",
        "IPN" => "i pe ene",
        "INE" => "ine",
        "CFE" => "ce efe e",
        "PEMEX" => "pemex",
        "FGR" => "efe ge erre",
        "PGR" => "pe ge erre",
        "UMA" => "uma",
        "UDI" | "UDIS" => "udis",
        "MXN" => "pesos",
        "USD" => "dólares",
        "PDF" => "pe de efe",
        "IA" => "i a",
        "OK" => "okey",
        "DOF" => "de o efe",
        "TSJ" => "te ese jota",
        "PROFECO" => "profeco",
        "CONDUSEF" => "condusef",
        "SA" => "ese a",
        "CV" => "ce ve",
        "EUA" => "Estados Unidos",
        _ => return None,
    })
}

fn letra(c: char) -> &'static str {
    match c.to_ascii_uppercase() {
        'A' => "a", 'B' => "be", 'C' => "ce", 'D' => "de", 'E' => "e", 'F' => "efe", 'G' => "ge", 'H' => "hache",
        'I' => "i", 'J' => "jota", 'K' => "ka", 'L' => "ele", 'M' => "eme", 'N' => "ene", 'Ñ' => "eñe", 'O' => "o",
        'P' => "pe", 'Q' => "cu", 'R' => "erre", 'S' => "ese", 'T' => "te", 'U' => "u", 'V' => "ve", 'W' => "doble ve",
        'X' => "equis", 'Y' => "ye", 'Z' => "zeta", _ => "",
    }
}

fn deletrear(s: &str) -> String {
    s.chars().map(letra).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

fn sigla_desconocida(s: &str) -> String {
    let vocales = s.chars().filter(|c| "AEIOU".contains(*c)).count();
    // pronunciable: tiene vocales y no junta tres consonantes («CONAGUA» sí, «SCT» no)
    let mut seguidas = 0;
    let mut max_seguidas = 0;
    for c in s.chars() {
        seguidas = if "AEIOU".contains(c) { 0 } else { seguidas + 1 };
        max_seguidas = max_seguidas.max(seguidas);
    }
    if s.chars().count() >= 4 && vocales > 0 && max_seguidas < 3 { s.to_lowercase() } else { deletrear(s) }
}

fn romano(s: &str) -> Option<u64> {
    if s.is_empty() || !s.chars().all(|c| "IVXLC".contains(c)) {
        return None;
    }
    let v = |c| match c { 'I' => 1, 'V' => 5, 'X' => 10, 'L' => 50, _ => 100 };
    let cs: Vec<u64> = s.chars().map(v).collect();
    let mut total = 0;
    for (i, &x) in cs.iter().enumerate() {
        if cs.get(i + 1).is_some_and(|&y| y > x) { total -= x as i64 } else { total += x as i64 }
    }
    (total > 0).then_some(total as u64)
}

// ---------------------------------------------------------------- teléfonos

/// «55 1234 5678», «55-1234-5678», «+52 55 1234 5678» → cifras de dos en dos.
fn telefonos(l: &str) -> String {
    let cs: Vec<char> = l.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        let inicio_ok = i == 0 || !(cs[i - 1].is_alphanumeric() || matches!(cs[i - 1], ',' | '.' | '$' | '/'));
        if inicio_ok && (cs[i].is_ascii_digit() || (cs[i] == '+' && cs.get(i + 1).is_some_and(|c| c.is_ascii_digit()))) {
            // recorre grupos de cifras separados por un espacio, guion o punto
            let mut j = i + usize::from(cs[i] == '+');
            let mut grupos: Vec<String> = Vec::new();
            loop {
                let a = j;
                while j < cs.len() && cs[j].is_ascii_digit() {
                    j += 1;
                }
                if j == a {
                    break;
                }
                grupos.push(cs[a..j].iter().collect());
                if j + 1 < cs.len() && matches!(cs[j], ' ' | '-' | '.') && cs[j + 1].is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            let fin_ok = j == cs.len() || !cs[j].is_alphanumeric();
            let mut cifras: String = grupos.concat();
            let mas = cs[i] == '+';
            let internacional = mas && cifras.starts_with("52") && cifras.len() == 12;
            let grupos_ok = grupos.iter().all(|g| (2..=4).contains(&g.len())) && grupos.len() >= 2;
            if fin_ok && (cifras.len() == 10 || internacional) && (grupos_ok || grupos.len() == 1) {
                let mut partes = Vec::new();
                if internacional {
                    partes.push("más cincuenta y dos".to_string());
                    cifras = cifras[2..].to_string();
                }
                for par in cifras.as_bytes().chunks(2) {
                    let s = std::str::from_utf8(par).unwrap();
                    partes.push(if s.starts_with('0') { digitos(s) } else { cardinal(s.parse().unwrap(), Genero::Suelto) });
                }
                out.push_str(&partes.join(" "));
                i = j;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- por palabra

struct Pieza {
    pre: String,
    nucleo: String,
    post: String,
}

fn partir(p: &str) -> Pieza {
    let es_pre = |c: char| matches!(c, '(' | '¿' | '¡' | '"' | '«' | '\'' | '[' | '“');
    let es_post = |c: char| matches!(c, ',' | ';' | ':' | '!' | '?' | ')' | '»' | '"' | '\'' | ']' | '”' | '.' | '…');
    let ini = p.char_indices().find(|(_, c)| !es_pre(*c)).map_or(p.len(), |(i, _)| i);
    let resto = &p[ini..];
    let fin = resto.char_indices().rev().find(|(_, c)| !es_post(*c)).map_or(0, |(i, c)| i + c.len_utf8());
    Pieza { pre: p[..ini].to_string(), nucleo: resto[..fin].to_string(), post: resto[fin..].to_string() }
}

fn limpio(p: &str) -> String {
    partir(p).nucleo.to_lowercase()
}

/// Quita el primer punto de `post` (el de la abreviatura), salvo que cierre la oración.
fn sin_punto(post: &str, final_de_linea: bool) -> String {
    if final_de_linea { post.to_string() } else { post.strip_prefix('.').unwrap_or(post).to_string() }
}

fn capitalizar(original: &str, inicio: bool, s: String) -> String {
    if inicio && original.starts_with(char::is_uppercase) {
        let mut c = s.chars();
        c.next().map_or(s.clone(), |f| f.to_uppercase().collect::<String>() + c.as_str())
    } else {
        s
    }
}

fn hora(h: u64, m: u64) -> String {
    let hs = cardinal(h, Genero::Fem);
    match m {
        0 => hs,
        1..=9 => format!("{hs} y {}", cardinal(m, Genero::Suelto)),
        _ => format!("{hs} {}", cardinal(m, Genero::Suelto)),
    }
}

fn fecha(d: u64, m: u64, a: Option<u64>) -> Option<String> {
    if !(1..=31).contains(&d) || !(1..=12).contains(&m) {
        return None;
    }
    let dia = if d == 1 { "primero".to_string() } else { cardinal(d, Genero::Suelto) };
    let mut s = format!("{dia} de {}", MESES[m as usize - 1]);
    if let Some(a) = a {
        let a = if a < 100 { 2000 + a } else { a };
        s.push_str(&format!(" de {}", cardinal(a, Genero::Suelto)));
    }
    Some(s)
}

fn linea(l: &str, mayusculas: bool) -> String {
    let ps: Vec<&str> = l.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < ps.len() {
        let p = partir(ps[i]);
        let n = p.nucleo.as_str();
        let siguiente = ps.get(i + 1).map(|s| limpio(s)).unwrap_or_default();
        let anterior = out.last().map(|s| limpio(s)).unwrap_or_default();
        let final_de_linea = i + 1 == ps.len();
        let pegado = p.post.is_empty(); // sin puntuación entre esta palabra y la siguiente
        let mut saltar = 0; // palabras siguientes ya consumidas
        let nuevo: Option<String> = (|| {
            // enlaces, correos, rutas y código: los trata para_voz
            if n.contains("://") || n.contains('@') || n.starts_with("www.") || n.contains(['\\', '_', '~', '`', '=', '{', '<'])
                || (n.contains('/') && n.chars().any(char::is_alphabetic))
            {
                return None;
            }
            // montos: «$1,160.00», «$3.5 millones», «US$500»
            let (moneda, cifra) = if let Some(r) = n.strip_prefix("US$") {
                (Some(("dólar", "dólares")), r)
            } else if let Some(r) = n.strip_prefix('$') {
                (Some(("peso", "pesos")), r)
            } else {
                (None, n)
            };
            if let Some((sing, plur)) = moneda {
                let cifra = if cifra.is_empty() && pegado { saltar = 1; limpio(ps.get(i + 1)?) } else { cifra.to_string() };
                let num = numero(&cifra)?;
                let mut k = i + 1 + saltar;
                let mut sufijo = None;
                if pegado || saltar > 0 {
                    let w = ps.get(k).map(|s| limpio(s)).unwrap_or_default();
                    if matches!(w.as_str(), "mil" | "millón" | "millones" | "mdp") {
                        sufijo = Some(if w == "mdp" { "millones".to_string() } else { w });
                        k += 1;
                    }
                }
                let mut moneda_plural = plur;
                let ultimo_k = k;
                let w = ps.get(k).map(|s| limpio(s)).unwrap_or_default();
                let w2 = ps.get(k + 1).map(|s| limpio(s)).unwrap_or_default();
                let w_orig = ps.get(k).map(|s| partir(s).nucleo).unwrap_or_default();
                let prev_pegado = ps.get(k - 1).is_some_and(|s| partir(s).post.is_empty());
                if prev_pegado && (matches!(w.as_str(), "pesos" | "peso" | "mxn" | "m.n" | "dólares" | "usd") || w_orig == "M.N") {
                    if w == "dólares" || w == "usd" {
                        moneda_plural = "dólares";
                    }
                    k += 1;
                } else if prev_pegado && w == "de" && matches!(w2.as_str(), "pesos" | "dólares") {
                    k += 2;
                }
                saltar = k - i - 1;
                // la puntuación tras la última palabra consumida se conserva
                let post = if k > ultimo_k { partir(ps[k - 1]).post } else if saltar > 0 { partir(ps[k - 1]).post } else { p.post.clone() };
                let mut s = if let Some(suf) = &sufijo {
                    let base = match &num.dec {
                        Some(d) => format!("{} punto {}", cardinal(num.entero, Genero::Suelto), decimales(d)),
                        None if suf == "millones" && num.entero == 1 => "un".to_string(),
                        None => cardinal(num.entero, Genero::Masc),
                    };
                    let suf = if suf == "millones" && num.entero == 1 && num.dec.is_none() { "millón" } else { suf.as_str() };
                    let de = if suf.starts_with("mill") { " de" } else { "" };
                    format!("{base} {suf}{de} {moneda_plural}")
                } else {
                    let exacto_millon = num.entero >= 1_000_000 && num.entero % 1_000_000 == 0;
                    let palabra = if num.entero == 1 { sing } else { moneda_plural };
                    let de = if exacto_millon { " de" } else { "" };
                    let mut s = format!("{}{de} {palabra}", cardinal(num.entero, Genero::Masc));
                    if let Some(d) = num.dec.as_deref().filter(|d| d.trim_end_matches('0') != "") {
                        let c: u64 = format!("{d:0<2}")[..2].parse().unwrap_or(0);
                        let cent = if c == 1 { "centavo" } else { "centavos" };
                        s.push_str(&format!(" con {} {cent}", cardinal(c, Genero::Masc)));
                    }
                    s
                };
                s = format!("{}{s}{post}", p.pre);
                return Some(s);
            }
            // porcentajes: «16%», «16 %»
            if let Some(c) = n.strip_suffix('%') {
                let num = numero(c)?;
                return Some(format!("{}{} por ciento{}", p.pre, leer_numero(&num, Genero::Suelto), p.post));
            }
            if pegado && siguiente == "%" {
                if let Some(num) = numero(n) {
                    saltar = 1;
                    return Some(format!("{}{} por ciento{}", p.pre, leer_numero(&num, Genero::Suelto), partir(ps[i + 1]).post));
                }
            }
            // horas: «10:30», «9:05 hrs.»
            if let Some((h, m)) = n.split_once(':') {
                if let (Ok(h), Ok(m)) = (h.parse::<u64>(), m.parse::<u64>()) {
                    if h <= 24 && m < 60 && n.len() <= 5 && n.split(':').nth(1).is_some_and(|x| x.len() == 2) {
                        let mut s = hora(h, m);
                        let mut post = p.post.clone();
                        if pegado {
                            match siguiente.as_str() {
                                "hrs" | "hr" | "h" | "horas" => {
                                    s.push_str(" horas");
                                    saltar = 1;
                                    post = sin_punto(&partir(ps[i + 1]).post, i + 2 == ps.len());
                                }
                                "a.m" | "am" => {
                                    s.push_str(" de la mañana");
                                    saltar = 1;
                                    post = sin_punto(&partir(ps[i + 1]).post, i + 2 == ps.len());
                                }
                                "p.m" | "pm" => {
                                    s.push_str(if h < 7 || h == 12 { " de la tarde" } else { " de la noche" });
                                    saltar = 1;
                                    post = sin_punto(&partir(ps[i + 1]).post, i + 2 == ps.len());
                                }
                                _ => {}
                            }
                        }
                        return Some(format!("{}{s}{post}", p.pre));
                    }
                }
            }
            // fechas y números con diagonal: «15/11/2026», «345/2026»; rangos: «5-7»; ISO: «2026-11-15»
            for sep in ['/', '-'] {
                let partes: Vec<&str> = n.split(sep).collect();
                if partes.len() < 2 || !partes.iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit())) {
                    continue;
                }
                let v: Vec<u64> = partes.iter().map(|x| x.parse().unwrap_or(0)).collect();
                let s = match (sep, v.as_slice()) {
                    ('-', [a, m, d]) if partes[0].len() == 4 => fecha(*d, *m, Some(*a)),
                    (_, [d, m, a]) => fecha(*d, *m, Some(*a)),
                    ('/', [d, m]) if partes[1].len() <= 2 => fecha(*d, *m, None),
                    _ => None,
                }
                .unwrap_or_else(|| {
                    let union = if sep == '/' { " diagonal " } else { " a " };
                    v.iter().map(|x| cardinal(*x, Genero::Suelto)).collect::<Vec<_>>().join(union)
                });
                return Some(format!("{}{s}{}", p.pre, p.post));
            }
            // ordinales: «1o.», «1º», «3er», «2da», «4to»
            let (cifras, suf): (String, String) = (n.chars().take_while(char::is_ascii_digit).collect(), n.chars().skip_while(char::is_ascii_digit).collect());
            if !cifras.is_empty() && !suf.is_empty() && !suf.contains(|c: char| c.is_ascii_digit() || c == ',') {
                let k: u64 = cifras.parse().ok()?;
                let ord = match suf.as_str() {
                    "o" | "º" | "°" | "ro" | "do" | "to" | "mo" | "vo" | "no" | "avo" | "ero" if k <= 20 => Some((false, false)),
                    "er" if k <= 20 => Some((false, true)),
                    "a" | "ª" | "ra" | "da" | "ta" | "ma" | "va" | "na" if k <= 20 => Some((true, false)),
                    _ => None,
                };
                if let Some((fem, apocope)) = ord {
                    let s = ordinal(k, fem, apocope);
                    return Some(format!("{}{s}{}", p.pre, sin_punto(&p.post, final_de_linea)));
                }
                // cifra con unidad pegada: «10km», «30°C»
                if let Some((s1, s2)) = unidad(&suf.to_lowercase()) {
                    let u = if k == 1 { s1 } else { s2 };
                    let g = if es_femenino(u.split(' ').next().unwrap_or("")) { Genero::Fem } else { Genero::Masc };
                    return Some(format!("{}{} {u}{}", p.pre, cardinal(k, g), p.post));
                }
                return None;
            }
            // versiones: «1.13.8»
            if n.matches('.').count() >= 2 && n.split('.').all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit())) {
                let s = n.split('.').map(|x| cardinal(x.parse().unwrap_or(0), Genero::Suelto)).collect::<Vec<_>>().join(" punto ");
                return Some(format!("{}{s}{}", p.pre, p.post));
            }
            // negativos
            if let Some(r) = n.strip_prefix('-') {
                let num = numero(r)?;
                return Some(format!("{}menos {}{}", p.pre, leer_numero(&num, Genero::Suelto), p.post));
            }
            // cifras sueltas
            if let Some(num) = numero(n) {
                let sig_orig = ps.get(i + 1).map(|s| partir(s).nucleo).unwrap_or_default();
                // «art. 4» → «artículo cuarto» (del 1 al 9, como se leen los artículos)
                if matches!(anterior.as_str(), "artículo" | "artículos") && num.dec.is_none() && (1..=9).contains(&num.entero) {
                    return Some(format!("{}{}{}", p.pre, ordinal(num.entero, false, false), p.post));
                }
                // «1 de noviembre» → «primero de noviembre»
                if num.entero == 1 && num.dec.is_none() && siguiente == "de" && ps.get(i + 2).is_some_and(|s| MESES.contains(&limpio(s).as_str())) {
                    return Some(format!("{}primero{}", p.pre, p.post));
                }
                // unidades: «10 km», «2 hrs»
                if pegado && num.dec.is_none() {
                    if let Some((s1, s2)) = unidad(&siguiente).filter(|_| sig_orig.len() <= 4) {
                        let u = if num.entero == 1 { s1 } else { s2 };
                        let g = if es_femenino(u.split(' ').next().unwrap_or("")) { Genero::Fem } else { Genero::Masc };
                        saltar = 1;
                        let post = sin_punto(&partir(ps[i + 1]).post, i + 2 == ps.len());
                        return Some(format!("{}{} {u}{post}", p.pre, cardinal(num.entero, g)));
                    }
                }
                // género por el sustantivo que sigue («21 días», «una hora», «doscientas páginas»)
                let sustantivo = if siguiente == "mil" { ps.get(i + 2).map(|s| limpio(s)).unwrap_or_default() } else { siguiente.clone() };
                let g = if !pegado || sustantivo.is_empty() || !sustantivo.starts_with(char::is_alphabetic) || NO_SUSTANTIVO.contains(&siguiente.as_str()) && siguiente != "mil" {
                    Genero::Suelto
                } else if es_femenino(&sustantivo) {
                    Genero::Fem
                } else {
                    Genero::Masc
                };
                let mut s = leer_numero(&num, g);
                if num.dec.is_none() && num.entero >= 1_000_000 && num.entero % 1_000_000 == 0 && g != Genero::Suelto {
                    s.push_str(" de");
                }
                return Some(format!("{}{s}{}", p.pre, p.post));
            }
            // abreviaturas: «art.», «Lic.», «p. ej.»
            if p.post.starts_with('.') {
                let a = n.to_lowercase();
                if a == "p" && siguiente == "ej" {
                    saltar = 1;
                    return Some(format!("{}por ejemplo{}", p.pre, sin_punto(&partir(ps[i + 1]).post, i + 2 == ps.len())));
                }
                if a == "etc" {
                    return Some(format!("{}etcétera{}", p.pre, p.post));
                }
                if matches!(a.as_str(), "no" | "núm" | "num") && !siguiente.starts_with(|c: char| c.is_ascii_digit()) {
                    return None;
                }
                if a == "ee.uu" {
                    return Some(format!("{}Estados Unidos{}", p.pre, sin_punto(&p.post, final_de_linea)));
                }
                if let Some(e) = abreviatura(&a, &anterior).or_else(|| (a == "no" || a == "n.º").then_some("número")) {
                    let fin = final_de_linea && !matches!(a.as_str(), "art" | "arts" | "lic" | "licda" | "dr" | "dra" | "sr" | "sra" | "ing");
                    return Some(format!("{}{}{}", p.pre, capitalizar(n, out.last().is_none_or(|w: &String| w.ends_with(['.', '?', '!', ':'])), e.to_string()), sin_punto(&p.post, fin)));
                }
                // siglas con puntos: «S.A. de C.V.»
                let letras: Vec<&str> = n.split('.').collect();
                if letras.len() >= 2 && letras.iter().all(|x| x.chars().count() == 1 && x.chars().all(char::is_uppercase)) {
                    return Some(format!("{}{}{}", p.pre, deletrear(&letras.concat()), sin_punto(&p.post, final_de_linea)));
                }
            }
            // números romanos tras «fracción», «título», «capítulo», «siglo»…
            if let Some(r) = romano(n) {
                match anterior.as_str() {
                    "fracción" | "fracciones" => return Some(format!("{}{}{}", p.pre, ordinal(r, true, false), p.post)),
                    "título" | "capítulo" | "libro" | "tomo" | "apartado" | "anexo" => {
                        return Some(format!("{}{}{}", p.pre, ordinal(r, false, false), p.post));
                    }
                    "siglo" | "sala" | "tribunal" | "circuito" | "distrito" => {
                        return Some(format!("{}{}{}", p.pre, cardinal(r, Genero::Suelto), p.post));
                    }
                    _ => {}
                }
            }
            // siglas en mayúsculas (salvo que todo el texto venga en mayúsculas)
            if !mayusculas && n.chars().count() >= 2 && n.chars().all(|c| c.is_uppercase()) {
                if let Some(s) = sigla(n) {
                    return Some(format!("{}{s}{}", p.pre, p.post));
                }
                let s = sigla_desconocida(n);
                if romano(n).is_none() && (n.chars().count() <= 6 || !s.contains(' ')) {
                    return Some(format!("{}{s}{}", p.pre, p.post));
                }
            }
            if n == "&" {
                return Some(format!("{}y{}", p.pre, p.post));
            }
            if n == "+" {
                return Some("más".into());
            }
            None
        })();
        out.push(nuevo.unwrap_or_else(|| ps[i].to_string()));
        i += 1 + saltar;
    }
    out.join(" ")
}

fn leer_numero(n: &Numero, g: Genero) -> String {
    match &n.dec {
        Some(d) => format!("{} punto {}", cardinal(n.entero, Genero::Suelto), decimales(d)),
        None => cardinal(n.entero, g),
    }
}

#[cfg(test)]
mod pruebas {
    use super::normalizar as n;

    #[test]
    fn cardinales() {
        assert_eq!(n("Tengo 0, 1, 15, 21, 100, 101 y 999."), "Tengo cero, uno, quince, veintiuno, cien, ciento uno y novecientos noventa y nueve.");
        assert_eq!(n("Son 1160 y 2026"), "Son mil ciento sesenta y dos mil veintiséis");
        assert_eq!(n("1000000 de personas"), "un millón de personas");
        assert_eq!(n("en 2,500,000 años"), "en dos millones quinientos mil años");
        assert_eq!(n("mide 3.5 y 0.75"), "mide tres punto cinco y cero punto setenta y cinco");
    }

    #[test]
    fn genero() {
        assert_eq!(n("1 hora y 21 días"), "una hora y veintiún días");
        assert_eq!(n("200 páginas, 1 peso"), "doscientas páginas, un peso");
        assert_eq!(n("21 audiencias"), "veintiuna audiencias");
        assert_eq!(n("el 1 y el 2"), "el uno y el dos");
        assert_eq!(n("300 mil personas"), "trescientas mil personas");
    }

    #[test]
    fn montos_y_porcentajes() {
        assert_eq!(n("El total con IVA es de $1,160.00 pesos."), "El total con iva es de mil ciento sesenta pesos.");
        assert_eq!(n("Se fijó una pensión de $12,500 mensuales."), "Se fijó una pensión de doce mil quinientos pesos mensuales.");
        assert_eq!(n("Cuesta $1,160.50"), "Cuesta mil ciento sesenta pesos con cincuenta centavos");
        assert_eq!(n("son $1 y $3,000,000"), "son un peso y tres millones de pesos");
        assert_eq!(n("asciende a $3.5 millones de pesos."), "asciende a tres punto cinco millones de pesos.");
        assert_eq!(n("son $500 MXN o US$20."), "son quinientos pesos o veinte dólares.");
        assert_eq!(n("La tasa es del 16 %."), "La tasa es del dieciséis por ciento.");
        assert_eq!(n("pagó el 50% del anticipo"), "pagó el cincuenta por ciento del anticipo");
    }

    #[test]
    fn horas_fechas_telefonos() {
        assert_eq!(n("La audiencia es a las 10:30 en la sala 2."), "La audiencia es a las diez treinta en la sala dos.");
        assert_eq!(n("a las 9:05 hrs. empieza"), "a las nueve y cinco horas empieza");
        assert_eq!(n("a las 1:00 p.m."), "a las una de la tarde.");
        assert_eq!(n("vence el 15/11/2026."), "vence el quince de noviembre de dos mil veintiséis.");
        assert_eq!(n("desde el 2026-01-01"), "desde el primero de enero de dos mil veintiséis");
        assert_eq!(n("el 1 de mayo"), "el primero de mayo");
        assert_eq!(n("El expediente 345/2026 está listo."), "El expediente trescientos cuarenta y cinco diagonal dos mil veintiséis está listo.");
        assert_eq!(n("páginas 5-7"), "páginas cinco a siete");
        assert_eq!(n("Llámale al 55 1234 5678."), "Llámale al cincuenta y cinco doce treinta y cuatro cincuenta y seis setenta y ocho.");
        assert_eq!(n("tel. 55-1234-0678"), "teléfono cincuenta y cinco doce treinta y cuatro cero seis setenta y ocho");
        assert_eq!(n("+52 55 1234 5678"), "más cincuenta y dos cincuenta y cinco doce treinta y cuatro cincuenta y seis setenta y ocho");
    }

    #[test]
    fn juridico() {
        assert_eq!(n("Según el art. 14 constitucional"), "Según el artículo catorce constitucional");
        assert_eq!(n("el artículo 1o. de la ley"), "el artículo primero de la ley");
        assert_eq!(n("el art. 4 dice"), "el artículo cuarto dice");
        assert_eq!(n("la fracción III del art. 27"), "la fracción tercera del artículo veintisiete");
        assert_eq!(n("Título IV, siglo XXI"), "Título cuarto, siglo veintiuno");
        assert_eq!(n("la Lic. Pérez y el Lic. Gómez"), "la licenciada Pérez y el licenciado Gómez");
        assert_eq!(n("Grupo Acme, S.A. de C.V."), "Grupo Acme, ese a de ce ve.");
        assert_eq!(n("el 3er lugar y la 2a instancia"), "el tercer lugar y la segunda instancia");
        assert_eq!(n("p. ej. esto, etc."), "por ejemplo esto, etcétera.");
        assert_eq!(n("No. 45 y No."), "Número cuarenta y cinco y No.");
    }

    #[test]
    fn siglas() {
        assert_eq!(n("ante el SAT y el IMSS"), "ante el sat y el i eme ese ese");
        assert_eq!(n("su RFC y CURP"), "su erre efe ce y curp");
        assert_eq!(n("la SCT y CONAGUA"), "la ese ce te y conagua");
        assert_eq!(n("ESTO ES UN AVISO MUY IMPORTANTE PARA TODOS"), "ESTO ES UN AVISO MUY IMPORTANTE PARA TODOS");
    }

    #[test]
    fn no_toca_rutas_ni_enlaces() {
        assert_eq!(n("ve a https://ejemplo.mx/2026/a y /home/u/2026/x.txt"), "ve a https://ejemplo.mx/2026/a y /home/u/2026/x.txt");
        assert_eq!(n("escribe a ana@ejemplo.mx"), "escribe a ana@ejemplo.mx");
        assert_eq!(n("versión 1.13.8 y F5"), "versión uno punto trece punto ocho y F5");
        assert_eq!(n("10 km y 2 hrs."), "diez kilómetros y dos horas.");
        assert_eq!(n("uno\ndos 2"), "uno\ndos dos");
    }
}
