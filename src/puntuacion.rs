//! Puntuación dictada: «coma», «punto y aparte», «abre interrogación»… → signos.
//!
//! Trabaja sobre la frase final del ASR. Si el ASR ya puso un signo junto a la palabra
//! dictada, gana el dictado. Lleva estado entre frases para saber si la siguiente
//! empieza oración (mayúscula) y cómo espaciarla.

/// (palabras habladas, signo). Las más largas primero para que «punto y coma» gane a «punto».
const COMANDOS: &[(&[&str], &str)] = &[
    (&["nueva", "viñeta", "numerada"], NUMERADA),
    (&["dos", "puntos", "y", "aparte"], ":\n"),
    (&["punto", "y", "a", "parte"], ".\n"),
    (&["abre", "signo", "de", "interrogación"], "¿"),
    (&["cierra", "signo", "de", "interrogación"], "?"),
    (&["abre", "signo", "de", "exclamación"], "¡"),
    (&["cierra", "signo", "de", "exclamación"], "!"),
    (&["punto", "y", "aparte"], ".\n"),
    (&["punto", "y", "seguido"], "."),
    // variantes que el ASR produce al oír los comandos de arriba
    (&["punto", "y", "seguida"], "."),
    (&["punto", "ya", "parte"], ".\n"),
    (&["cambio", "de", "renglón"], "\n"),
    (&["salto", "de", "línea"], "\n"),
    (&["punto", "y", "coma"], ";"),
    (&["abre", "interrogación"], "¿"),
    (&["cierra", "interrogación"], "?"),
    (&["abre", "exclamación"], "¡"),
    (&["cierra", "exclamación"], "!"),
    (&["abre", "paréntesis"], "("),
    (&["cierra", "paréntesis"], ")"),
    (&["abre", "comillas"], "“"),
    (&["cierra", "comillas"], "”"),
    (&["puntos", "suspensivos"], "..."),
    (&["punto", "final"], "."),
    (&["dos", "puntos"], ":"),
    (&["nuevo", "párrafo"], "\n\n"),
    (&["viñeta", "numerada"], NUMERADA),
    (&["nueva", "viñeta"], "\n- "),
    (&["viñeta"], "\n- "),
    (&["nueva", "línea"], "\n"),
    (&["nuevo", "renglón"], "\n"),
    (&["otro", "renglón"], "\n"),
    (&["siguiente", "renglón"], "\n"),
    (&["punto", "aparte"], ".\n"),
    (&["punto", "seguido"], "."),
    (&["punto"], "."),
    (&["coma"], ","),
];

/// Comandos de formato Markdown (modo para IureEditor u otros editores que lo interpretan).
/// Las negritas y cursivas usan marcas internas para decidir espacios; al final son `**` y `*`.
const MARKDOWN: &[(&[&str], &str)] = &[
    (&["título", "uno"], "\n# "),
    (&["título", "dos"], "\n## "),
    (&["título", "tres"], "\n### "),
    (&["abre", "negritas"], "\u{2}"),
    (&["cierra", "negritas"], "\u{3}"),
    (&["abre", "cursivas"], "\u{4}"),
    (&["cierra", "cursivas"], "\u{5}"),
    (&["bloque", "de", "cita"], "\n> "),
    (&["línea", "horizontal"], "\n---\n"),
    (&["casilla"], "\n- [ ] "),
];

/// Marca de «viñeta numerada»: se sustituye por «\n1. », «\n2. »… según el contador.
const NUMERADA: &str = "\u{1}";

/// Antes de «punto»/«coma», estas palabras indican que es sustantivo («el punto», «en coma»).
const DETERMINANTES: &[&str] = &[
    "el", "la", "un", "una", "este", "esta", "ese", "esa", "aquel", "al", "del", "en", "cada",
    "ningún", "algún", "mismo", "último", "primer", "buen", "mal", "otro", "otra", "su", "mi",
    "primero", "segundo", "tercer", "tercero", "cuarto", "quinto", "sexto", "séptimo", "octavo",
    "noveno", "décimo", "siguiente", "próximo", "anterior", "nuestro", "tu", "dicho", "ese", "aquel",
];

/// Signos que se pegan a la palabra anterior / a la siguiente.
fn pega_izquierda(s: &str) -> bool {
    matches!(s.chars().next(), Some(',' | '.' | ';' | ':' | '?' | '!' | ')' | '”' | '…' | '\n' | '\u{3}' | '\u{5}'))
}
fn pega_derecha(s: &str) -> bool {
    // «\n- » y «\n1. » ya traen su espacio
    matches!(s.chars().last(), Some('¿' | '¡' | '(' | '“' | '\n' | ' ' | '\u{2}' | '\u{4}'))
}
fn cierra_oracion(s: &str) -> bool {
    s.ends_with(' ') || matches!(s.trim_end_matches([' ', '”', ')']).chars().last(), Some('.' | '?' | '!' | '…' | '\n'))
}

fn limpia(p: &str) -> String {
    let w = p.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    // plurales y lo que suele oír el reconocedor («Vinientas numeradas»): cuentan como el comando
    match w.as_str() {
        "viñetas" | "vinietas" | "vinieta" | "vinientas" | "vinienta" | "vineta" | "vinetas" | "viñieta" | "biñeta" | "biñetas" => {
            "viñeta".into()
        }
        "numeradas" | "numerado" | "numerados" => "numerada".into(),
        _ => w,
    }
}

fn mayuscula_inicial(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(p) => p.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[derive(Default)]
pub struct Puntuador {
    /// Lo emitido hasta ahora termina oración (o aún no hay nada).
    fin_oracion: bool,
    /// Lo emitido termina en un signo que pide pegar lo siguiente («¿», «(», salto).
    pegar: bool,
    iniciado: bool,
    /// Último número usado en una lista numerada; se reinicia con cada párrafo.
    numero: u32,
    /// Comandos de formato Markdown activos.
    pub markdown: bool,
}

impl Puntuador {
    pub fn new() -> Self {
        Self { fin_oracion: true, pegar: true, iniciado: false, numero: 0, markdown: false }
    }

    /// Resuelve la viñeta numerada y reinicia la numeración en cada párrafo.
    fn expandir(&mut self, signo: &str) -> String {
        if signo == NUMERADA {
            self.numero += 1;
            format!("\n{}. ", self.numero)
        } else {
            if signo.contains('\n') {
                self.numero = 0; // punto y aparte o párrafo: termina la lista
            }
            signo.to_string()
        }
    }

    pub fn reiniciar(&mut self) {
        let markdown = self.markdown;
        *self = Self::new();
        self.markdown = markdown;
    }

    /// Convierte una frase y devuelve el texto a insertar, con el espacio inicial
    /// que haga falta para continuar lo anterior.
    pub fn procesar(&mut self, frase: &str, dictada: bool) -> String {
        let palabras: Vec<&str> = frase.split_whitespace().collect();
        let limpias: Vec<String> = palabras.iter().map(|p| limpia(p)).collect();
        // piezas: (texto, es_signo)
        let mut piezas: Vec<(String, bool)> = Vec::new();
        // Un comando exacto de varias palabras gana, salvo que la frase entera se parezca a uno
        // más largo: «viñeta numerada dos» es viñeta + «dos»; «dos puntos si aparte» es
        // «dos puntos y aparte».
        let l = &limpias;
        let exacto_mayor = (0..limpias.len())
            .flat_map(|i| {
                COMANDOS.iter().filter(move |(h, _)| {
                    i + h.len() <= l.len() && l[i..i + h.len()].iter().zip(h.iter()).all(|(a, b)| a == b)
                })
            })
            .map(|(h, _)| h.len())
            .max()
            .unwrap_or(0);
        let aproximado = if dictada {
            comando_aproximado(&limpias).filter(|(n, _)| *n > exacto_mayor.max(1)).map(|(_, s)| s)
        } else {
            None
        };
        let mut i = if aproximado.is_some() { palabras.len() } else { 0 };
        if let Some(signo) = aproximado {
            piezas.push((self.expandir(signo), true));
        }
        'fuera: while i < palabras.len() {
            if dictada {
                let extra: &[(&[&str], &str)] = if self.markdown { MARKDOWN } else { &[] };
                for (habla, signo) in extra.iter().chain(COMANDOS) {
                    let n = habla.len();
                    if i + n <= palabras.len() && limpias[i..i + n].iter().zip(habla.iter()).all(|(a, b)| a == b) {
                        if es_sustantivo(&limpias, i, n) {
                            break;
                        }
                        // el dictado manda: quita el signo que el ASR haya pegado a la palabra previa
                        if let Some((ult, false)) = piezas.last_mut() {
                            let recortada = ult.trim_end_matches([',', '.', ';', ':']).to_string();
                            *ult = recortada;
                        }
                        piezas.push((self.expandir(signo), true));
                        i += n;
                        continue 'fuera;
                    }
                }
            }
            piezas.push((palabras[i].to_string(), false));
            i += 1;
        }

        let mut salida = String::new();
        let mut fin = self.fin_oracion;
        let mut pegar = self.pegar;
        for (texto, es_signo) in piezas {
            if texto.is_empty() {
                continue;
            }
            let espacio = !(pegar || (es_signo && pega_izquierda(&texto)) || (!self.iniciado && salida.is_empty()));
            if espacio {
                salida.push(' ');
            }
            if es_signo {
                salida.push_str(&texto);
                // «¿» y «¡» abren oración también
                fin = cierra_oracion(&texto) || (fin && pega_derecha(&texto));
                pegar = pega_derecha(&texto);
            } else {
                let t = if fin { mayuscula_inicial(&texto) } else { texto };
                fin = cierra_oracion(&t);
                pegar = false;
                salida.push_str(&t);
            }
            self.iniciado = true;
        }
        self.fin_oracion = fin;
        self.pegar = pegar;
        // comillas tipográficas solo para decidir espacios: se escriben rectas, que todo
        // teclado puede producir
        salida.replace(['“', '”'], "\"").replace(['\u{2}', '\u{3}'], "**").replace(['\u{4}', '\u{5}'], "*")
    }
}

/// «el punto de vista», «en coma», «en el nuevo párrafo»: el comando se usa como sustantivo.
fn es_sustantivo(limpias: &[String], i: usize, n: usize) -> bool {
    let previa = i.checked_sub(1).map(|j| limpias[j].as_str());
    let siguiente = limpias.get(i + n).map(|s| s.as_str());
    // para comandos de varias palabras solo cuentan los artículos: «primero punto y coma» sí es comando
    let articulos = ["el", "la", "los", "las", "un", "una", "unos", "unas", "este", "esta", "ese", "esa", "del", "al"];
    let det = if n == 1 { DETERMINANTES } else { &articulos[..] };
    previa.is_some_and(|p| det.contains(&p)) || (n == 1 && matches!(siguiente, Some("de" | "del" | "medio")))
}

fn distancia(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + usize::from(x != y)));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Si la frase entera se parece a un comando de varias palabras («Tun ya parte», «salto que
/// línea»), se toma como ese comando. Solo con la frase completa: en medio de un texto
/// normal sería demasiado arriesgado.
fn comando_aproximado(limpias: &[String]) -> Option<(usize, &'static str)> {
    if limpias.is_empty() || limpias.len() > 5 {
        return None;
    }
    let frase: Vec<char> = limpias.concat().chars().collect();
    COMANDOS
        .iter()
        .filter(|(habla, _)| habla.len() >= 2)
        .map(|(habla, signo)| {
            let c: Vec<char> = habla.concat().chars().collect();
            (distancia(&frase, &c) as f32 / c.len() as f32, c.len(), habla.len(), *signo)
        })
        .filter(|(d, largo, _, _)| *largo >= 8 && *d <= 0.26)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, _, n, signo)| (n, signo))
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn una(frase: &str) -> String {
        Puntuador::new().procesar(frase, true)
    }

    #[test]
    fn signos_basicos() {
        assert_eq!(una("hola coma qué tal punto"), "Hola, qué tal.");
        assert_eq!(una("primero punto y coma segundo dos puntos tercero"), "Primero; segundo: tercero");
        assert_eq!(una("abre interrogación vienes cierra interrogación"), "¿Vienes?");
        assert_eq!(una("dijo abre comillas sí cierra comillas punto"), "Dijo \"sí\".");
        assert_eq!(una("bueno puntos suspensivos ya"), "Bueno... Ya");
    }

    #[test]
    fn mayuscula_tras_punto_y_aparte() {
        assert_eq!(una("fin punto y aparte otra cosa"), "Fin.\nOtra cosa");
    }

    #[test]
    fn vinetas() {
        let mut p = Puntuador::new();
        assert_eq!(p.procesar("pendientes dos puntos viñeta revisar contrato", true), "Pendientes:\n- Revisar contrato");
        assert_eq!(p.procesar("viñeta llamar al cliente", true), "\n- Llamar al cliente");
    }

    #[test]
    fn markdown() {
        let mut p = Puntuador::new();
        p.markdown = true;
        assert_eq!(p.procesar("título uno informe de avance", true), "\n# Informe de avance");
        assert_eq!(p.procesar("es abre negritas urgente cierra negritas punto", true), " es **urgente**.");
        assert_eq!(Puntuador::new().procesar("título uno informe", true), "Título uno informe");
    }

    #[test]
    fn lista_numerada() {
        let mut p = Puntuador::new();
        assert_eq!(p.procesar("elementos dos puntos viñeta numerada uno", true), "Elementos:\n1. Uno");
        assert_eq!(p.procesar("viñeta numerada dos", true), "\n2. Dos");
        // plurales y variantes del reconocedor
        assert_eq!(p.procesar("Vinientas numeradas tres", true), "\n3. Tres");
        assert_eq!(Puntuador::new().procesar("viñetas comprar pan", true), "\n- Comprar pan");
        assert_eq!(
            Puntuador::new().procesar("si bien las viñetas numeradas no me han funcionado", true),
            "Si bien las viñetas numeradas no me han funcionado"
        );
        assert_eq!(p.procesar("fin punto y aparte viñeta numerada otra", true), " fin.\n\n1. Otra");
    }

    #[test]
    fn variantes_del_asr() {
        assert_eq!(una("bien Punto y seguida sigue"), "Bien. Sigue");
        assert_eq!(una("bien Punto ya parte sigue"), "Bien.\nSigue");
        assert_eq!(una("elementos dos puntos y aparte uno"), "Elementos:\nUno");
        assert_eq!(una("hola cambio de renglón adiós"), "Hola\nAdiós");
    }

    #[test]
    fn comandos_mal_oidos_como_frase_completa() {
        let mut p = Puntuador::new();
        assert_eq!(p.procesar("hola", true), "Hola");
        assert_eq!(p.procesar("Tun ya parte", true), ".\n");
        assert_eq!(una("Salto que línea"), "\n");
        assert_eq!(una("Punto y en parte"), ".\n");
        assert_eq!(una("dos puntos si aparte"), ":\n");
        // texto normal no se toca
        assert_eq!(una("Siguiente escenario"), "Siguiente escenario");
        assert_eq!(una("Tres manzanas"), "Tres manzanas");
        assert_eq!(una("puede parte"), "Puede parte");
    }

    #[test]
    fn comando_como_sustantivo() {
        assert_eq!(una("en el nuevo párrafo esperamos"), "En el nuevo párrafo esperamos");
        assert_eq!(Puntuador::new().procesar("Nuevo párrafo", true), "\n\n");
    }

    #[test]
    fn ordinales_con_punto() {
        assert_eq!(una("segundo punto revisar"), "Segundo punto revisar");
        assert_eq!(una("tercer punto"), "Tercer punto");
    }

    #[test]
    fn sustantivos_no_son_comandos() {
        assert_eq!(una("desde mi punto de vista llegó a las cinco en punto"), "Desde mi punto de vista llegó a las cinco en punto");
        assert_eq!(una("el paciente está en coma punto"), "El paciente está en coma.");
    }

    #[test]
    fn el_dictado_gana_al_signo_del_asr() {
        assert_eq!(una("Asimismo, coma manifiesta."), "Asimismo, manifiesta.");
    }

    #[test]
    fn continuidad_entre_frases() {
        let mut p = Puntuador::new();
        assert_eq!(p.procesar("hola coma", true), "Hola,");
        assert_eq!(p.procesar("Qué tal punto", true), " Qué tal.");
        assert_eq!(p.procesar("adiós", true), " Adiós");
        assert_eq!(p.procesar("nueva línea sigue", true), "\nSigue");
    }

    #[test]
    fn sin_dictado_respeta_el_texto() {
        assert_eq!(Puntuador::new().procesar("hola coma", false), "Hola coma");
    }
}
