//! Modo comando: un LLM interpreta la instrucción hablada y llama herramientas de una
//! lista cerrada (`escritorio`). Nunca hay shell ni comandos arbitrarios.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{Correccion as LlmCfg, Servicio};
use crate::escritorio as esc;
use crate::portal::Portal;
use crate::voz::Voz;

const MAX_PASOS: usize = 5;

fn sistema() -> String {
    format!(
        "Eres IureVoz, un asistente que controla el escritorio Linux (GNOME) de quien te habla. \
La instrucción viene de un reconocedor de voz y puede tener errores: interpreta la intención. \
Usa SOLO las herramientas disponibles. Si para hacerlo hay que trabajar en el sistema (crear o editar un \
script o programa, ejecutar comandos, instalar o configurar, mover, copiar, renombrar o borrar archivos) usa \
pedir_a_claude si está disponible; nunca lo intentes tecleando en una terminal. Si no está, di que no puedes. \
Si la instrucción menciona archivos, carpetas, PDF, escritos, grabaciones, juntas, transcripciones o el editor, \
usa expedientes_y_apps (son archivos de este equipo, nunca de internet). Para proyectos, tareas, vencimientos o \
expedientes en Iurefficient usa consultar_iurefficient. \
Nunca inventes URLs, cifras ni resultados: solo cuenta lo que devolvió una herramienta. \
Si la instrucción suena a abrir algo (aunque esté mal escrita, p. ej. «apre» o «habre»), usa abrir_aplicacion. \
Nunca digas que hiciste o harás algo sin haber llamado la herramienta correspondiente. \
Para cualquier cuenta usa la herramienta calcular, nunca calcules de memoria. \
Si te preguntan algo de conocimiento general, responde tú en una o dos frases; busca en la web solo si lo piden. \
Si preguntan qué puedes hacer: dictar en cualquier ventana (con puntuación, viñetas y «borra la palabra»), \
abrir apps y páginas, buscar en internet, leer encabezados de noticias; consultar Iurefficient (proyectos, tareas, \
agenda, horas, CRM) y registrar horas, tareas o eventos; buscar y leer archivos de IureDav, OCR de PDF, grabar \
y transcribir juntas con IureTranscribe, escribir en IureEditor; y pasarle a Claude tareas del sistema como scripts. \
Cuando termines responde en español de México (tutea; nunca uses voseo), en una o dos frases cortas, porque tu respuesta se lee en voz alta. \
No uses markdown ni listas. La persona está en México: moneda en pesos, IVA del 16 %. \
Si una cuenta es ambigua, haz la interpretación más común y dila en la respuesta. Si la instrucción completa o responde a algo de los mensajes anteriores \
(p. ej. solo un número tras preguntar «¿de qué cantidad?»), úsala junto con ese contexto. \
Hoy es {} (para la hora exacta usa hora_fecha).",
        // solo la fecha: con la hora el prefijo cambiaría cada minuto y no serviría la caché
        esc::hora_fecha().rsplit_once(',').map_or_else(|| esc::hora_fecha(), |(f, _)| f.to_string())
    )
}

fn sistema_suite() -> String {
    "Eres el ayudante de IureVoz para las apps de escritorio de la suite Iurefficient: IureDav (unidad de \
archivos de los expedientes), IureOCR (PDF), IureTranscribe (grabar y transcribir reuniones) e IureEditor \
(documentos). «Transcribe la junta (o reunión, audiencia, llamada)» que va a empezar o está por empezar \
es grabarla con grabar_reunion: IureTranscribe la transcribe al detenerla. Si piden transcribir una grabación \
que ya existe, usa grabaciones_recientes y transcribir. Resuelve el pedido con las herramientas; para encontrar un archivo usa primero archivos \
(listar o buscar) y luego usa la ruta exacta que devuelva. Si te piden el contenido o un resumen, lee el \
archivo y resume tú en dos o tres frases. Al crear un documento no preguntes el título: si no lo dieron, se pone uno con la fecha. Nunca inventes rutas ni resultados. Responde en español de México (tutea; nunca voseo), breve, \
sin markdown, porque se lee en voz alta."
        .to_string()
}

fn sistema_iure() -> String {
    format!(
        "Eres el ayudante de IureVoz para consultar la instancia web de Iurefficient de quien te habla: \
proyectos y expedientes, sus tareas, vencimientos y documentos, las oportunidades y prospectos del CRM \
(consultar_crm), y su día: sus tareas (mis_tareas), su agenda, \
sus notificaciones, sus horas registradas (mis_horas) y clientes (buscar_cliente). Las herramientas son de solo lectura. \
Para «qué tengo hoy» combina mis_tareas y agenda. \
También puedes escribir, siempre con confirmación del usuario (la pide la herramienta, no la pidas tú): \
registrar_horas, crear_tarea, crear_evento, marcar_notificaciones_leidas. Las fechas van como AAAA-MM-DD \
calculadas desde hoy («mañana», «el jueves») y las horas como HH:MM de 24 horas. \
Si la herramienta responde que el usuario no lo permitió, dilo y no insistas. \
Para hablar de un proyecto por su nombre, búscalo primero (listar_proyectos con búsqueda) y usa su id. \
Si piden abrir o ver algo en Iurefficient, usa abrir_en_iurefficient. \
Responde en español de México (tutea; nunca voseo), breve (dos o tres frases), sin markdown ni listas largas, porque se lee en voz alta; \
resume y da lo más importante primero (fechas próximas, pendientes). Nunca inventes datos. Hoy es {}.",
        esc::hora_fecha().rsplit_once(',').map_or_else(|| esc::hora_fecha(), |(f, _)| f.to_string())
    )
}

/// Sub-agentes: el principal delega para no cargar a los modelos chicos con todas las herramientas.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Modo {
    Principal,
    /// Apps de escritorio de la suite (archivos, PDF, grabaciones, editor).
    Suite,
    /// Instancia web de Iurefficient por su servidor MCP.
    Iure,
}

/// Herramientas del sub-agente de la suite.
fn herramientas_suite() -> Value {
    let f = |nombre: &str, desc: &str, props: Value, req: &[&str]| {
        json!({"type": "function", "function": {"name": nombre, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": req}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    json!([
        f("archivos", "Lista una carpeta de la unidad de IureDav (expedientes) o busca archivos por nombre.", json!({"carpeta": s("carpeta relativa a la unidad; vacío = raíz"), "buscar": s("palabras del nombre del archivo")}), &[]),
        f("abrir_archivo", "Abre un archivo con su programa, o en IureEditor si con_editor es true.", json!({"ruta": s("ruta devuelta por archivos o ruta en la carpeta personal"), "con_editor": {"type": "boolean"}}), &["ruta"]),
        f("nuevo_documento", "Crea un documento nuevo (Markdown en Documentos) y lo abre en IureEditor para escribir o dictar.", json!({"titulo": s("título o nombre del documento (opcional)")}), &[]),
        f("leer_archivo", "Devuelve el texto de un archivo (PDF, txt, md) para resumirlo o responder sobre él.", json!({"ruta": s("ruta"), "desde_pagina": n("PDF: primera página"), "hasta_pagina": n("PDF: última página")}), &["ruta"]),
        f("leer_en_voz_alta", "Lee en voz alta el texto completo de un archivo, si el usuario pide que se lo lean.", json!({"ruta": s("ruta")}), &["ruta"]),
        f("ocr_pdf", "Pasa OCR a un PDF escaneado o imagen con IureOCR y crea un PDF con texto junto al original.", json!({"ruta": s("ruta"), "idiomas": s("p. ej. spa+eng")}), &["ruta"]),
        f("info_pdf", "Cuántas páginas tiene un PDF y si ya tiene texto.", json!({"ruta": s("ruta")}), &["ruta"]),
        f("editar_paginas_pdf", "Crea una copia del PDF quitando, conservando, girando o reordenando páginas. Pide confirmación.", json!({"ruta": s("ruta"), "operacion": {"type": "string", "enum": ["delete", "keep", "rotate", "reorder"]}, "paginas": {"type": "array", "items": {"type": "integer"}}, "grados": n("para rotate: 90, 180 o 270")}), &["ruta", "operacion", "paginas"]),
        f("grabar_reunion", "Empieza a grabar una reunión en IureTranscribe.", json!({"participantes": s("nombres separados por coma"), "lugar": s("lugar"), "notas": s("tema o notas")}), &[]),
        f("detener_grabacion", "Detiene y guarda la grabación en curso de IureTranscribe.", json!({}), &[]),
        f("grabaciones_recientes", "Lista las grabaciones recientes de IureTranscribe (rutas y fechas).", json!({"limite": n("cuántas, 5 por omisión")}), &[]),
        f("transcribir", "Transcribe un audio con IureTranscribe y devuelve el texto.", json!({"ruta": s("ruta del audio")}), &["ruta"]),
        f("leer_transcripcion", "Devuelve el texto de una transcripción ya hecha.", json!({"ruta": s("ruta del audio o de la transcripción")}), &["ruta"]),
    ])
}

fn herramienta_crm() -> Value {
    json!({"type": "function", "function": {"name": "consultar_crm",
        "description": "Oportunidades de venta abiertas o prospectos (leads) del CRM de Iurefficient: nombre, organización, etapa, monto, probabilidad y cierre esperado.",
        "parameters": {"type": "object", "properties": {
            "tipo": {"type": "string", "enum": ["oportunidades", "prospectos"]},
            "busqueda": {"type": "string", "description": "texto a buscar (cliente, organización, nombre)"},
            "solo_mias": {"type": "boolean", "description": "solo las asignadas al usuario"}},
            "required": ["tipo"]}}})
}

fn herramienta_abrir_iure() -> Value {
    json!({"type": "function", "function": {"name": "abrir_en_iurefficient",
        "description": "Abre Iurefficient en el navegador: un proyecto (por id o nombre), una tarea (por id), o la agenda, los vencimientos, la hoja de horas, las notificaciones, las tareas o el inicio.",
        "parameters": {"type": "object", "properties": {
            "que": {"type": "string", "enum": ["proyecto", "tarea", "agenda", "vencimientos", "horas", "notificaciones", "tareas", "inicio"]},
            "id": {"type": "string", "description": "id del proyecto o la tarea, si se conoce"},
            "nombre": {"type": "string", "description": "nombre del proyecto, si no hay id"}},
            "required": ["que"]}}})
}

/// «Ábrelo», «ábrela», «abre eso»: abrir lo último mencionado (se decide en código).
pub fn es_abrelo(instruccion: &str) -> bool {
    let t: String = crate::correccion::palabras(instruccion).join(" ");
    matches!(
        t.as_str(),
        "ábrelo" | "abrelo" | "ábrela" | "abrela" | "abre eso" | "abre ese" | "abre esa" | "ábrelo por favor" | "ábrela por favor"
            | "ábrelo en el navegador" | "ábrela en el navegador" | "muéstramelo" | "muéstramela"
    )
}

/// Acciones que el agente necesita del resto de la app.
pub struct Ganchos {
    /// Pide permiso al usuario (voz y tarjeta): (descripción hablada) → sí/no.
    pub confirmar: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Pausa (true) o reanuda (false) «siempre escuchando», p. ej. mientras se graba una junta.
    pub pausar_escucha: Box<dyn Fn(bool) + Send + Sync>,
    /// Empieza (true) o termina (false) el dictado, como el botón.
    pub dictar: Box<dyn Fn(bool) + Send + Sync>,
    /// El dictado va a este documento de IureEditor por enlace (`None`: a la ventana activa).
    pub destino: Box<dyn Fn(Option<std::path::PathBuf>) + Send + Sync>,
}

/// «Te voy a dictar», «vamos a escribir», «toma dictado»: empezar a dictar (se decide en código).
pub fn es_dictar(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    !es_fin_dictado(&t)
        && ["dictar", "dictado", "vamos a escribir", "voy a escribir", "a escribir unas", "toma nota", "toma notas", "escribe lo que te diga", "escribe lo que diga"]
            .iter()
            .any(|f| t.contains(f))
}

/// «Termina el dictado», «deja de dictar», «ya acabé de dictar». Tolera errores del
/// reconocedor en frases cortas («Termina Victila»).
pub fn es_fin_dictado(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    if crate::correccion::palabras(&t).len() <= 4
        && ["termina el dictado", "termina dictado", "terminar dictado", "deja de dictar", "detén el dictado", "detener dictado"]
            .iter()
            .any(|f| crate::vigilia::coincide(&t, f, 0.4).is_some())
    {
        return true;
    }
    ["termina el dictado", "terminar el dictado", "deja de dictar", "detener dictado", "detener el dictado", "terminar dictado",
     "termina dictado", "detén dictado", "deten dictado", "fin del dictado", "para de dictar", "detén el dictado",
     "deten el dictado", "acabé de dictar", "acabe de dictar", "terminé de dictar", "termine de dictar", "para el dictado", "termina de dictar"]
        .iter()
        .any(|f| t.contains(f))
}

/// ¿Además de dictar pide abrir algo (editor, documento)? Entonces primero se abre.
fn pide_abrir(t: &str) -> bool {
    let t = t.to_lowercase();
    ["abre", "abrir", "editor", "documento", "archivo"].iter().any(|f| t.contains(f))
}

fn herramientas(con_claude: bool) -> Value {
    let f = |nombre: &str, desc: &str, props: Value, req: &[&str]| {
        json!({"type": "function", "function": {"name": nombre, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": req}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let mut v = json!([
        f("abrir_aplicacion", "Abre una aplicación instalada por su nombre (p. ej. calculadora, Firefox, Archivos, terminal).", json!({"nombre": s("nombre de la aplicación")}), &["nombre"]),
        f("abrir_url", "Abre una dirección web en el navegador.", json!({"url": s("URL o dominio")}), &["url"]),
        f("buscar_web", "Busca algo en internet y abre los resultados en el navegador.", json!({"consulta": s("qué buscar")}), &["consulta"]),
        f("abrir_carpeta", "Abre una carpeta en el gestor de archivos: documentos, descargas, escritorio, imágenes, música, videos, inicio, o una ruta dentro de la carpeta personal.", json!({"nombre": s("carpeta")}), &["nombre"]),
        f("calcular", "Evalúa una expresión aritmética (+ - * / ^ %, paréntesis, sqrt). Devuelve el resultado.", json!({"expresion": s("expresión, p. ej. (25*4)+7")}), &["expresion"]),
        f("teclas", "Pulsa una combinación de teclas en la ventana activa, p. ej. ctrl+t (nueva pestaña), ctrl+w, alt+tab, super, ctrl+l, alt+f4, f11.", json!({"combinacion": s("teclas unidas con +")}), &["combinacion"]),
        f("escribir", "Escribe un texto en la ventana activa, como si se tecleara.", json!({"texto": s("texto a escribir")}), &["texto"]),
        f("volumen", "Cambia el volumen del sistema.", json!({"accion": {"type": "string", "enum": ["subir", "bajar", "silenciar", "activar"]}, "cantidad": {"type": "integer", "description": "porcentaje, 10 por omisión"}}), &["accion"]),
        f("hora_fecha", "Da la fecha y hora actuales.", json!({}), &[]),
        f("leer_seleccion", "Lee en voz alta el texto seleccionado (o el del portapapeles).", json!({}), &[]),
        f("leer_pagina", "Lee en voz alta el texto completo de un artículo web concreto, solo si el usuario pide que se lo lean entero. Nunca con páginas de buscadores.", json!({"url": s("URL del artículo")}), &["url"]),
        f("consultar_pagina", "Trae el texto de una página web (p. ej. la portada de un periódico) para que tú la resumas o digas sus encabezados. Usa la URL oficial del sitio (nytimes.com, eluniversal.com.mx…).", json!({"url": s("URL de la página")}), &["url"]),
        f("expedientes_y_apps", "Pedidos sobre las apps de Iurefficient en este equipo: archivos de expedientes en IureDav (listar, buscar, abrir, leer, resumir), PDF con IureOCR (OCR, leer páginas, girar o quitar páginas), grabar o transcribir reuniones con IureTranscribe, abrir o crear documentos en IureEditor.", json!({"pedido": s("el pedido completo, con nombres de archivo o carpeta tal como se dijeron")}), &["pedido"]),
    ]);
    if con_claude {
        v.as_array_mut().unwrap().push(f(
            "pedir_a_claude",
            "Pasa la tarea a Claude, un asistente con acceso completo a la computadora, para lo que las demás herramientas no pueden: manejar o buscar archivos, instalar o configurar programas, ejecutar comandos, preguntas complejas. El usuario confirma antes.",
            json!({"tarea": s("la tarea completa, en español")}),
            &["tarea"],
        ));
    }
    v
}

/// Pide confirmación y, si se concede, lanza la tarea con Claude. Devuelve qué pasó.
pub type Delegar = Box<dyn Fn(&str) -> Result<String> + Send + Sync>;

/// «Abre Google Chrome», «inicia la calculadora»: el nombre de la app a abrir, si la frase
/// no es más que eso (las apps de la suite, la web y el dictado tienen su propio camino).
pub fn app_a_abrir(instruccion: &str) -> Option<String> {
    let t = instruccion.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
    const VERBOS: &[&str] = &["abre la aplicación ", "abre la app ", "ábreme ", "abreme ", "abre ", "abrir ", "inicia ", "lanza ", "arranca "];
    let resto = VERBOS.iter().find_map(|v| t.strip_prefix(v))?.trim();
    if resto.is_empty() || resto.split_whitespace().count() > 4 {
        return None;
    }
    // «google chrome» y «el navegador» son apps, no un pedido de web
    let sin_apps = t.replace("google chrome", "chrome").replace("navegador", "");
    if es_de_la_suite(&t) || pide_web(&sin_apps) || es_de_iurefficient(&t) || es_dictar(&t) || es_abrelo(&t) {
        return None;
    }
    Some(resto.to_string())
}

/// Nombres de la suite como suelen llegar del reconocedor («Euroficient», «Yureditor»):
/// cada palabra a pocas letras de un nombre se reemplaza por el nombre correcto.
pub fn normalizar_nombres(instruccion: &str) -> String {
    const NOMBRES: &[(&str, &str, usize)] = &[
        ("iurefficient", "Iurefficient", 3),
        ("iureditor", "IureEditor", 2),
        ("iuredav", "IureDav", 2),
        ("iuretranscribe", "IureTranscribe", 3),
        ("iureocr", "IureOCR", 2),
    ];
    let plano = |w: &str| -> Vec<char> {
        w.to_lowercase()
            .chars()
            .map(|c| match c {
                'á' => 'a',
                'é' => 'e',
                'í' => 'i',
                'ó' => 'o',
                'ú' => 'u',
                c => c,
            })
            .filter(|c| c.is_alphanumeric())
            .collect()
    };
    instruccion
        .split(' ')
        .map(|w| {
            let p = plano(w);
            if p.len() < 6 {
                return w.to_string();
            }
            // la «y» y la «eu» iniciales son la «iu» mal oída
            let mut alt = p.clone();
            if alt.first() == Some(&'y') {
                alt[0] = 'i';
            }
            if alt.starts_with(&['e', 'u']) {
                alt[0] = 'i';
            }
            for (clave, bien, max) in NOMBRES {
                let c: Vec<char> = clave.chars().collect();
                if crate::puntuacion::distancia(&alt, &c) <= *max {
                    let fin: String = w.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<Vec<_>>().into_iter().rev().collect();
                    return format!("{bien}{fin}");
                }
            }
            w.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// ¿Es trabajo de sistema que las herramientas básicas no hacen (scripts, comandos, instalar,
/// configurar, mover o renombrar archivos)? Va a Claude Code. Se decide en código: el modelo
/// tiende a intentarlo con «teclas» y «escribir» o a decir que no puede.
pub fn es_tarea_de_sistema(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    const CLAVES: &[&str] = &[
        "script", "programa que", "programita", "python", "bash", "comando", "terminal y ejecuta",
        "ejecuta ", "instala", "desinstala", "configura", "actualiza el sistema", "actualiza los paquetes",
        "renombra", "mueve los", "mueve el", "mueve la", "copia los", "copia el", "copia la", "borra los", "borra el",
        "borra la", "elimina los", "comprime", "descomprime", "respalda", "respaldo", "cron", "servicio de",
        "permisos de", "espacio en disco", "procesos", "git ", "repositorio", "compila",
    ];
    CLAVES.iter().any(|c| t.contains(c))
}

/// ¿La instrucción habla de archivos o apps de la suite? Se decide en código: los modelos
/// chicos confunden «busca el archivo X» con buscar en internet.
pub fn es_de_la_suite(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    const CLAVES: &[&str] = &[
        "archivo", "documento", "carpeta", "pdf", "escrito", "contrato", "grabaci", "grabar",
        "graba ", "junta", "reunión", "reunion", "transcrib", "transcripc", "editor", "iuredav", "iureocr",
        "iuretranscribe", "iureditor", "ocr", " unidad", "página ", "páginas",
    ];
    let web = pide_web(&t);
    !web && CLAVES.iter().any(|c| t.contains(c))
}

/// Palabras que indican archivos locales (IureDav, PDF…) y no la instancia web.
const PALABRAS_ARCHIVO: &[&str] = &["archivo", "carpeta", "pdf", "grabaci", "transcrip", "editor", "ocr", " unidad", "página"];

/// ¿La instrucción es sobre la instancia web (proyectos, tareas, vencimientos, expedientes)?
pub fn es_de_iurefficient(instruccion: &str) -> bool {
    let t = instruccion.to_lowercase();
    const CLAVES: &[&str] = &[
        "proyecto", "tarea", "pendiente", "vencimiento", "vence", "plazo", "agenda", "iurefficient", "expediente",
        "cómo va", "como va", "avance",
        // «mi día» (F2): tareas, agenda, avisos, horas y clientes de quien habla
        "qué tengo", "que tengo", "reunión", "reunion", "junta", "audiencia", "calendario", "notificaci", "aviso",
        "horas llevo", "mis horas", "horas registr", "hoja de horas", "cliente",
        // CRM (oportunidades y prospectos)
        "oportunidad", "crm", "prospecto", "embudo", "pipeline",
        // F4: escrituras
        "registra", "agend", "agénd", "evento", "cita ", "márca", "marca como leíd", "marca las",
    ];
    // «graba la junta», «qué tengo en Descargas»: eso es de las apps de escritorio
    const DE_ESCRITORIO: &[&str] = &["graba", "descargas", "documentos", "escritorio"];
    !pide_web(&t)
        && CLAVES.iter().any(|c| t.contains(c))
        && !PALABRAS_ARCHIVO.iter().any(|c| t.contains(c))
        && !DE_ESCRITORIO.iter().any(|c| t.contains(c))
}

/// ¿Pidió explícitamente internet? Sin esto, buscar_web y leer_pagina no se ejecutan.
fn pide_web(t: &str) -> bool {
    let t = t.to_lowercase();
    ["internet", "la web", "en web", "google", "en línea", "en linea", "navegador", "wikipedia", "sitio", "http", ".com", ".mx", "url",
     "página web", "página de", "página del", "periódico", "noticias", "encabezados", "titulares", "new york times", "diario "]
        .iter()
        .any(|c| t.contains(c))
}

fn primera_mayuscula(t: &str) -> String {
    let mut c = t.trim().trim_end_matches('.').chars();
    c.next().map(|p| p.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// A quién le toca una instrucción (lo decide `Agente::enrutar`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destino {
    /// La instancia web: proyectos, tareas, agenda, horas, CRM… (sub-agente de Iurefficient).
    Iurefficient,
    /// Apps de escritorio de la suite: IureDav, IureOCR, IureTranscribe, IureEditor.
    Apps,
    /// Trabajo en el sistema operativo: a Claude Code.
    Sistema,
    /// Herramientas básicas del escritorio (abrir, web, volumen, teclas…).
    Escritorio,
    /// Pregunta general o seguimiento de la charla.
    Conversacion,
}

const SISTEMA_ENRUTADOR: &str = "Clasificas instrucciones de voz para IureVoz, un asistente de escritorio en Linux. \
Responde SOLO una palabra de esta lista:\n\
iurefficient: la instancia web de Iurefficient de la persona: proyectos o expedientes, sus tareas y vencimientos, \
su agenda y calendario (reuniones, juntas, audiencias y citas agendadas), sus horas registradas, notificaciones, clientes, \
el CRM (oportunidades, prospectos), documentos guardados en la instancia; también registrar horas o crear tareas o eventos.\n\
apps: las apps de escritorio de la suite en este equipo: archivos de la unidad IureDav (buscar, abrir, leer, resumir), \
PDF (OCR, leer, girar o quitar páginas), GRABAR o TRANSCRIBIR una reunión, junta, audiencia o llamada con IureTranscribe \
(«transcribe la junta», «graba la reunión», «detén la grabación»), abrir o crear documentos en IureEditor.\n\
sistema: trabajo en el sistema operativo con comandos: crear o editar scripts o programas, instalar o configurar, \
mover, copiar, renombrar, comprimir o borrar archivos, procesos, espacio en disco.\n\
escritorio: abrir una aplicación o una página web, buscar en internet, noticias o encabezados de un sitio, volumen, \
pulsar teclas, escribir un texto, cuentas, la hora, leer en voz alta lo seleccionado.\n\
conversacion: preguntas de conocimiento general, charla, o «qué puedes hacer».\n\
Si la instrucción contesta o continúa la conversación previa, clasifícala por el tema de esa conversación.";

pub struct Agente {
    /// Instrucción en curso (para los candados de herramientas con efectos fuera).
    instruccion: std::sync::Mutex<String>,
    cfg: LlmCfg,
    agente: ureq::Agent,
    portal: Option<Portal>,
    voz: Option<Voz>,
    delegar: Option<Delegar>,
    ganchos: Option<Arc<Ganchos>>,
    modo: Modo,
}

/// Lo que pasó al ejecutar una instrucción.
#[derive(Debug, Clone, Default)]
pub struct Informe {
    pub acciones: Vec<String>,
    pub respuesta: String,
}

impl Agente {
    pub fn new(cfg: LlmCfg, portal: Option<Portal>, voz: Option<Voz>) -> Self {
        let agente = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).build().into();
        Self { instruccion: Default::default(), cfg, agente, portal, voz, delegar: None, ganchos: None, modo: Modo::Principal }
    }

    pub fn con_ganchos(mut self, g: Arc<Ganchos>) -> Self {
        self.ganchos = Some(g);
        self
    }

    fn definiciones(&self) -> Value {
        match self.modo {
            Modo::Principal => herramientas(self.delegar.is_some()),
            Modo::Suite => herramientas_suite(),
            // si no está conectado, sin herramientas: el modelo lo dirá con el error de abajo
            Modo::Iure => {
                let mut v = crate::iure::herramientas().unwrap_or_else(|_| json!([]));
                if let Some(a) = v.as_array_mut() {
                    a.push(herramienta_abrir_iure());
                    a.push(herramienta_crm());
                    a.extend(crate::escribir::herramientas());
                }
                v
            }
        }
    }

    fn prompt(&self) -> String {
        match self.modo {
            Modo::Principal => sistema(),
            Modo::Suite => sistema_suite(),
            Modo::Iure => sistema_iure(),
        }
    }

    pub fn con_claude(mut self, delegar: Delegar) -> Self {
        self.delegar = Some(delegar);
        self
    }

    /// ¿A qué parte de IureVoz le toca la instrucción? Una consulta corta al modelo con el
    /// catálogo de lo que se sabe hacer: las palabras clave solas se confunden («transcribe la
    /// junta» tiene «junta», que también es de la agenda de Iurefficient).
    fn enrutar(&self, instruccion: &str, historial: &[Value]) -> Result<Destino> {
        let previo: Vec<String> = historial
            .iter()
            .rev()
            .take(2)
            .rev()
            .filter_map(|m| Some(format!("{}: {}", m["role"].as_str()?, m["content"].as_str()?.chars().take(300).collect::<String>())))
            .collect();
        let usuario = if previo.is_empty() {
            format!("Instrucción: {instruccion}")
        } else {
            format!("Conversación previa:\n{}\n\nInstrucción: {instruccion}", previo.join("\n"))
        };
        let mensajes = [json!({"role": "system", "content": SISTEMA_ENRUTADOR}), json!({"role": "user", "content": usuario})];
        let (msg, _) = self.pedir(&mensajes, None)?;
        let t = msg["content"].as_str().unwrap_or("").to_lowercase();
        // la primera etiqueta que aparezca en la respuesta
        let mut mejor: Option<(usize, Destino)> = None;
        for (etiqueta, d) in [
            ("iurefficient", Destino::Iurefficient),
            ("apps", Destino::Apps),
            ("sistema", Destino::Sistema),
            ("escritorio", Destino::Escritorio),
            ("conversacion", Destino::Conversacion),
            ("conversación", Destino::Conversacion),
        ] {
            if let Some(i) = t.find(etiqueta) {
                if mejor.is_none_or(|(j, _)| i < j) {
                    mejor = Some((i, d));
                }
            }
        }
        mejor.map(|(_, d)| d).with_context(|| format!("el enrutador respondió «{}»", t.trim()))
    }

    /// Con el LLM local: arranca el servidor y deja en caché el prefijo (instrucciones y
    /// herramientas), para que la primera instrucción real no espere a procesarlo.
    pub fn precalentar(&self) -> Result<()> {
        if self.cfg.servicio != Servicio::Local {
            return Ok(());
        }
        let base = crate::llm_local::url(&self.cfg.modelo)?;
        let mensajes = json!([{"role": "system", "content": sistema()}, {"role": "user", "content": "hola"}]);
        let cuerpo = json!({"model": self.cfg.modelo, "temperature": 0, "max_tokens": 1, "messages": mensajes,
            "tools": self.definiciones(), "chat_template_kwargs": {"enable_thinking": false}});
        self.agente.post(&format!("{base}/chat/completions")).send_json(&cuerpo)?;
        Ok(())
    }

    /// `historial`: turnos recientes (usuario/asistente) para dar continuidad a la conversación.
    pub fn ejecutar(&self, instruccion: &str, historial: &[Value], mut paso: impl FnMut(&str)) -> Result<Informe> {
        // «Euroficient» → «Iurefficient», etc., antes de enrutar
        let normalizada = normalizar_nombres(instruccion);
        let instruccion = normalizada.as_str();
        *self.instruccion.lock().unwrap() = instruccion.to_string();
        // instancia web o apps de la suite: directo al sub-agente, sin pasar por el modelo principal
        if self.modo == Modo::Principal && es_fin_dictado(instruccion) {
            if let Some(g) = &self.ganchos {
                (g.dictar)(false);
            }
            paso("termina el dictado");
            return Ok(Informe { acciones: vec!["terminar dictado".into()], respuesta: "Listo, dejé de dictar.".into() });
        }
        if self.modo == Modo::Principal && es_dictar(instruccion) && !pide_abrir(instruccion) {
            if let Some(g) = &self.ganchos {
                (g.dictar)(true);
            }
            paso("empieza el dictado");
            return Ok(Informe {
                acciones: vec!["empezar dictado".into()],
                respuesta: "Adelante, te escucho. Para terminar di «oye compu, termina el dictado».".into(),
            });
        }
        if self.modo == Modo::Principal && crate::resumen::es_mi_dia(instruccion) {
            paso("resumen del día");
            let respuesta = match crate::resumen::mi_dia() {
                Ok(t) => t,
                Err(e) => format!("No pude consultar Iurefficient: {}.", format!("{e:#}").trim_end_matches('.')),
            };
            return Ok(Informe { acciones: vec!["resumen del día".into()], respuesta });
        }
        if self.modo == Modo::Principal && es_abrelo(instruccion) {
            let r = crate::iure::abrir_lo_ultimo();
            let texto = match &r {
                Ok(t) => t.clone(),
                Err(e) => format!("{e:#}"),
            };
            paso(&format!("abrir lo último → {texto}"));
            let respuesta = match r {
                Ok(t) => format!("Listo, {t}."),
                Err(e) => format!("{}.", format!("{e:#}").trim_end_matches('.')),
            };
            return Ok(Informe { acciones: vec![texto], respuesta });
        }
        // «abre Google Chrome»: se abre en código; si no hay una app con ese nombre, sigue el modelo
        if self.modo == Modo::Principal {
            if let Some(app) = app_a_abrir(instruccion) {
                if let Ok(t) = esc::abrir_aplicacion(&app) {
                    paso(&format!("abrir_aplicacion → {t}"));
                    return Ok(Informe { acciones: vec![format!("abrir_aplicacion({app})")], respuesta: format!("Listo, {t}.") });
                }
            }
        }
        // trabajo de sistema: a Claude Code (pide permiso antes de empezar)
        if self.modo == Modo::Principal && self.delegar.is_some() && es_tarea_de_sistema(instruccion) {
            paso("tarea de sistema → Claude");
            let r = self.delegar.as_ref().unwrap()(instruccion)?;
            return Ok(Informe { acciones: vec![format!("pedir_a_claude → {r}")], respuesta: primera_mayuscula(&r) + "." });
        }
        let directo = if self.modo != Modo::Principal {
            None
        } else {
            match self.enrutar(instruccion, historial) {
                Ok(d) => {
                    paso(&format!("enrutado: {d:?}"));
                    match d {
                        Destino::Iurefficient => Some("consultar_iurefficient"),
                        Destino::Apps => Some("expedientes_y_apps"),
                        Destino::Sistema if self.delegar.is_some() => {
                            let r = self.delegar.as_ref().unwrap()(instruccion)?;
                            return Ok(Informe { acciones: vec![format!("pedir_a_claude → {r}")], respuesta: primera_mayuscula(&r) + "." });
                        }
                        _ => None,
                    }
                }
                // sin respuesta del modelo: palabras clave
                Err(e) => {
                    paso(&format!("enrutador: {e:#}; se usan palabras clave"));
                    if es_de_iurefficient(instruccion) {
                        Some("consultar_iurefficient")
                    } else if es_de_la_suite(instruccion) {
                        Some("expedientes_y_apps")
                    } else {
                        None
                    }
                }
            }
        };
        if let Some(h) = directo {
            let editor_ya_abierto = es_dictar(instruccion) && esc::corre("iureditor");
            let texto = self.herramienta(h, &json!({"pedido": instruccion}))?;
            paso(&format!("{h} → {}", texto.chars().take(160).collect::<String>()));
            let mut respuesta = texto.split(" (hice: ").next().unwrap_or(&texto).trim().to_string();
            // «abre el editor y vamos a escribir»: abierto el documento, se dicta
            if self.modo == Modo::Principal && es_dictar(instruccion) && texto.contains("IureEditor") {
                if let Some(g) = &self.ganchos {
                    // con un IureEditor que acepta enlaces de insertar, el dictado entra directo al
                    // documento aunque su ventana esté detrás
                    let doc = match crate::iure::ultima() {
                        Some(crate::iure::Referencia::Archivo(p)) if crate::suite::editor_inserta() => Some(p),
                        _ => None,
                    };
                    let directo_al_doc = doc.is_some();
                    (g.destino)(doc);
                    (g.dictar)(true);
                    // la respuesta del sub-agente puede pedir datos («¿qué título?»): aquí ya no se pregunta.
                    // Si el editor ya estaba abierto, GNOME no le da el foco: hay que hacer clic.
                    respuesta = if directo_al_doc {
                        "Listo, abrí el documento en IureEditor. Dicta: el texto entra directo al documento; para terminar di «termina el dictado».".into()
                    } else if editor_ya_abierto {
                        "Abrí el documento en IureEditor. Haz clic en su ventana y dicta; para terminar di «termina el dictado».".into()
                    } else {
                        "Listo, abrí el documento en IureEditor. Ya puedes dictar; para terminar di «termina el dictado».".into()
                    };
                }
            }
            return Ok(Informe { acciones: vec![format!("{h}({})", args_cortos(&json!({"pedido": instruccion})))], respuesta });
        }
        if self.modo == Modo::Iure {
            // sin conexión no tiene sentido llamar al modelo
            crate::iure::herramientas()?;
        }
        let mut inf = Informe::default();
        let mut ultimo = String::new();
        // misma herramienta con los mismos datos: no se repite (modelos chicos a veces insisten)
        let mut hechas: Vec<(String, Value, String)> = Vec::new();
        let mut mensajes = vec![json!({"role": "system", "content": self.prompt()})];
        mensajes.extend(historial.iter().cloned());
        mensajes.push(json!({"role": "user", "content": instruccion}));
        for _ in 0..MAX_PASOS {
            let (msg, mut llamadas) = self.llamar(&mensajes)?;
            if llamadas.is_empty() {
                // modelos chicos a veces escriben la llamada como texto en vez de estructurarla
                llamadas = llamadas_en_texto(msg["content"].as_str().unwrap_or(""), &self.definiciones());
            }
            if llamadas.is_empty() {
                inf.respuesta = msg["content"].as_str().unwrap_or("").trim().to_string();
                if inf.respuesta.is_empty() && !ultimo.is_empty() {
                    // sin redacción del modelo: se informa el resultado de la última acción
                    inf.respuesta = format!("Listo: {ultimo}.");
                }
                return Ok(inf);
            }
            mensajes.push(msg.clone());
            for (id, nombre, args) in llamadas {
                let previa = hechas.iter().find(|(n, a, _)| *n == nombre && *a == args).map(|(_, _, r)| r.clone());
                let texto = match previa {
                    Some(r) => format!("ya hecho antes en esta instrucción ({r}); no lo repitas y responde"),
                    None => {
                        let t = match self.herramienta(&nombre, &args) {
                            Ok(t) => t,
                            Err(e) => format!("error: {e:#}"),
                        };
                        hechas.push((nombre.clone(), args.clone(), t.clone()));
                        t
                    }
                };
                ultimo = texto.clone();
                // en el registro de acciones basta el inicio del resultado (puede ser un archivo entero)
                let corto: String = texto.chars().take(160).collect();
                let corto = if texto.chars().count() > 160 { format!("{corto}…") } else { corto };
                let accion = format!("{nombre}({}) → {corto}", args_cortos(&args));
                paso(&accion);
                inf.acciones.push(accion);
                let mut r = json!({"role": "tool", "content": texto});
                if self.cfg.servicio == Servicio::Ollama {
                    r["tool_name"] = json!(nombre);
                } else {
                    r["tool_call_id"] = json!(id);
                }
                mensajes.push(r);
            }
        }
        inf.respuesta = "Hice lo que pude, pero la instrucción requirió demasiados pasos.".into();
        Ok(inf)
    }

    fn herramienta(&self, nombre: &str, a: &Value) -> Result<String> {
        let s = |k: &str| a[k].as_str().map(str::to_string).with_context(|| format!("falta «{k}»"));
        match nombre {
            "abrir_aplicacion" => esc::abrir_aplicacion(&s("nombre")?),
            "abrir_url" => esc::abrir_url(&s("url")?),
            "buscar_web" if !pide_web(&self.instruccion.lock().unwrap()) => {
                bail!("no se pidió buscar en internet; si se trata de archivos o expedientes usa expedientes_y_apps, y si es una pregunta general responde tú")
            }
            "buscar_web" => esc::buscar_web(&s("consulta")?),
            "abrir_carpeta" => esc::abrir_carpeta(&s("nombre")?),
            "calcular" => esc::calcular(&s("expresion")?),
            "teclas" => {
                let c = s("combinacion")?;
                let ks = esc::combinacion(&c)?;
                self.portal.as_ref().context("sin acceso al teclado")?.teclas(ks);
                Ok(format!("pulsé {c}"))
            }
            "escribir" => {
                let t = s("texto")?;
                self.portal.as_ref().context("sin acceso al teclado")?.escribir(t.clone());
                Ok(format!("escribí {} caracteres", t.chars().count()))
            }
            "volumen" => esc::volumen(&s("accion")?, a["cantidad"].as_u64().map(|n| n as u32)),
            "hora_fecha" => Ok(esc::hora_fecha()),
            "leer_seleccion" => {
                let t = esc::seleccion()?;
                self.voz.as_ref().context("la voz está desactivada")?.decir(t.clone());
                Ok(format!("leyendo {} palabras en voz alta", t.split_whitespace().count()))
            }
            "pedir_a_claude" => self.delegar.as_ref().context("Claude no está habilitado")?(&s("tarea")?),
            "expedientes_y_apps" => {
                // sub-agente con las herramientas de la suite: el prompt principal no crece
                let mut sub = Agente::new(self.cfg.clone(), None, self.voz.clone());
                sub.modo = Modo::Suite;
                sub.ganchos = self.ganchos.clone();
                let inf = sub.ejecutar(&s("pedido")?, &[], |_| {})?;
                let hecho = if inf.acciones.is_empty() { String::new() } else { format!(" (hice: {})", inf.acciones.join("; ")) };
                Ok(format!("{}{hecho}", inf.respuesta))
            }
            "consultar_iurefficient" => {
                let mut sub = Agente::new(self.cfg.clone(), None, self.voz.clone());
                sub.modo = Modo::Iure;
                // F4: las escrituras se confirman con los mismos ganchos
                sub.ganchos = self.ganchos.clone();
                let inf = sub.ejecutar(&s("pedido")?, &[], |_| {})?;
                let hecho = if inf.acciones.is_empty() { String::new() } else { format!(" (hice: {})", inf.acciones.join("; ")) };
                Ok(format!("{}{hecho}", inf.respuesta))
            }
            "consultar_crm" => crate::iure::crm(
                a["tipo"].as_str().unwrap_or("oportunidades"),
                a["busqueda"].as_str(),
                a["solo_mias"].as_bool().unwrap_or(false),
                20,
            ),
            "abrir_en_iurefficient" => crate::iure::abrir(&s("que")?, a["id"].as_str(), a["nombre"].as_str()),
            n if self.modo == Modo::Iure && crate::escribir::es_herramienta(n) => {
                let g = self.ganchos.clone();
                crate::escribir::ejecutar(n, a, &move |d: &str| g.as_ref().is_some_and(|g| (g.confirmar)(d)))
            }
            n if self.modo == Modo::Iure && crate::iure::es_herramienta(n) => crate::iure::llamar(n, a.clone()),
            "archivos" => crate::suite::archivos(a["carpeta"].as_str(), a["buscar"].as_str()),
            "abrir_archivo" => crate::suite::abrir(&s("ruta")?, a["con_editor"].as_bool().unwrap_or(false)),
            "nuevo_documento" => crate::suite::nuevo_documento(a["titulo"].as_str()),
            "leer_archivo" => crate::suite::texto(&s("ruta")?, a["desde_pagina"].as_u64(), a["hasta_pagina"].as_u64()),
            "leer_en_voz_alta" => {
                let t = crate::suite::texto(&s("ruta")?, None, None)?;
                self.voz.as_ref().context("la voz está desactivada")?.decir(t.clone());
                Ok(format!("leyendo {} palabras en voz alta", t.split_whitespace().count()))
            }
            "ocr_pdf" => crate::suite::ocr(&s("ruta")?, a["idiomas"].as_str()),
            "info_pdf" => crate::suite::info_pdf(&s("ruta")?),
            "editar_paginas_pdf" => {
                let ruta = s("ruta")?;
                let op = s("operacion")?;
                let paginas: Vec<u64> = a["paginas"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
                let grados = a["grados"].as_i64();
                let desc = crate::suite::describir_edicion(&ruta, &op, &paginas, grados);
                // crea un archivo nuevo: se confirma antes
                let ok = self.ganchos.as_ref().is_some_and(|g| (g.confirmar)(&desc));
                if !ok {
                    return Ok("el usuario no lo permitió".into());
                }
                crate::suite::editar_pdf(&ruta, &op, &paginas, grados)
            }
            "grabar_reunion" => {
                let r = crate::suite::grabar(a["participantes"].as_str(), a["lugar"].as_str(), a["notas"].as_str())?;
                // la junta no debe tomarse como instrucciones
                if let Some(g) = &self.ganchos {
                    (g.pausar_escucha)(true);
                }
                Ok(r)
            }
            "detener_grabacion" => {
                let r = crate::suite::detener_grabacion()?;
                if let Some(g) = &self.ganchos {
                    (g.pausar_escucha)(false);
                }
                Ok(r)
            }
            "grabaciones_recientes" => crate::suite::grabaciones(a["limite"].as_u64().unwrap_or(5)),
            "transcribir" => crate::suite::transcribir(&s("ruta")?),
            "leer_transcripcion" => crate::suite::leer_transcripcion(&s("ruta")?),
            "leer_pagina" if !pide_web(&self.instruccion.lock().unwrap()) && !self.instruccion.lock().unwrap().to_lowercase().contains("página") => {
                bail!("no se pidió leer una página web")
            }
            "consultar_pagina" => {
                let t = esc::texto_de_pagina(&s("url")?)?;
                Ok(t.chars().take(6000).collect())
            }
            "leer_pagina" => {
                let t = esc::texto_de_pagina(&s("url")?)?;
                self.voz.as_ref().context("la voz está desactivada")?.decir(t.clone());
                Ok(format!("leyendo la página ({} palabras)", t.split_whitespace().count()))
            }
            n => bail!("herramienta desconocida: {n}"),
        }
    }

    /// Devuelve el mensaje del asistente y sus llamadas: (id, nombre, argumentos).
    fn llamar(&self, mensajes: &[Value]) -> Result<(Value, Vec<(String, String, Value)>)> {
        self.pedir(mensajes, Some(self.definiciones()))
    }

    /// Una vuelta con el modelo; sin herramientas, solo texto (p. ej. para enrutar).
    fn pedir(&self, mensajes: &[Value], herramientas: Option<Value>) -> Result<(Value, Vec<(String, String, Value)>)> {
        let local;
        let base = if self.cfg.servicio == Servicio::Local {
            local = crate::llm_local::url(&self.cfg.modelo)?;
            local.as_str()
        } else {
            self.cfg.url.trim_end_matches('/')
        };
        let (url, cuerpo, clave) = match self.cfg.servicio {
            Servicio::Ollama => (
                format!("{base}/api/chat"),
                json!({"model": self.cfg.modelo, "stream": false, "think": false, "keep_alive": "30m",
                    "options": {"temperature": 0, "num_ctx": 4096}, "messages": mensajes, "tools": herramientas.clone().unwrap_or(json!([]))}),
                None,
            ),
            Servicio::OpenRouter | Servicio::OpenAi | Servicio::Local => {
                let mut c = json!({"model": self.cfg.modelo, "temperature": 0, "messages": mensajes});
                if let Some(h) = &herramientas {
                    c["tools"] = h.clone();
                }
                if self.cfg.servicio == Servicio::OpenRouter {
                    c["reasoning"] = json!({"enabled": false});
                } else {
                    // llama-server: sin modo de razonamiento (si no, lo escribe como respuesta)
                    c["chat_template_kwargs"] = json!({"enable_thinking": false});
                }
                let clave = self.cfg.clave_efectiva();
                if self.cfg.servicio == Servicio::OpenRouter && clave.is_none() {
                    bail!("falta la API key de OpenRouter");
                }
                (format!("{base}/chat/completions"), c, clave)
            }
        };
        let mut req = self.agente.post(&url).header("X-Title", "iurevoz");
        if let Some(c) = &clave {
            req = req.header("Authorization", &format!("Bearer {c}"));
        }
        let r: Value = req.send_json(&cuerpo).with_context(|| format!("POST {url}"))?.body_mut().read_json()?;
        let msg = if self.cfg.servicio == Servicio::Ollama { r["message"].clone() } else { r["choices"][0]["message"].clone() };
        let llamadas = msg["tool_calls"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|c| {
                        let f = &c["function"];
                        // OpenAI manda los argumentos como texto JSON; Ollama como objeto
                        let args = match &f["arguments"] {
                            Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                            v => v.clone(),
                        };
                        (c["id"].as_str().unwrap_or("").to_string(), f["name"].as_str().unwrap_or("").to_string(), args)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((msg, llamadas))
    }
}

/// Reconoce llamadas escritas como texto, p. ej. el formato nativo de Gemma:
/// `abrir_aplicacion{nombre:<|"|>Excel<|"|>}`, `calcular{expresion:"2+2"}` o al estilo
/// función: `calcular(1585 * 0.23)`, `abrir_url(url="…")`, `hora_fecha()`.
fn llamadas_en_texto(texto: &str, defs: &Value) -> Vec<(String, String, Value)> {
    let nombres: Vec<String> = defs
        .as_array()
        .map(|v| v.iter().filter_map(|h| h["function"]["name"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // primer parámetro (para llamadas con un solo argumento sin nombre)
    let primero = |n: &str| -> Option<String> {
        defs.as_array()?.iter().find(|h| h["function"]["name"] == n).and_then(|h| {
            let f = &h["function"]["parameters"];
            f["required"].get(0).or_else(|| f["properties"].as_object()?.keys().next().map(|_| &f["required"][0]))?.as_str().map(str::to_string)
        })
    };
    let mut out = Vec::new();
    for n in &nombres {
        let mut resto = texto;
        while let Some(i) = resto.find(&format!("{n}(")) {
            // que no sea parte de otra palabra
            if i > 0 && resto[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                resto = &resto[i + n.len()..];
                continue;
            }
            let ini = i + n.len() + 1;
            // paréntesis balanceados (la expresión puede llevarlos)
            let mut nivel = 1;
            let mut fin = None;
            for (k, c) in resto[ini..].char_indices() {
                match c {
                    '(' => nivel += 1,
                    ')' => {
                        nivel -= 1;
                        if nivel == 0 {
                            fin = Some(ini + k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(fin) = fin else { break };
            let cuerpo = resto[ini..fin].trim();
            let mut args = serde_json::Map::new();
            let con_nombre = cuerpo.split_once(['=', ':']).filter(|(k, _)| k.trim().chars().all(|c| c.is_alphanumeric() || c == '_') && !k.trim().is_empty());
            if let Some((k, v)) = con_nombre {
                args.insert(k.trim().to_string(), Value::from(v.trim().trim_matches(['"', '\''])));
            } else if !cuerpo.is_empty() {
                if let Some(p) = primero(n) {
                    args.insert(p, Value::from(cuerpo.trim_matches(['"', '\''])));
                }
            }
            out.push((String::new(), n.clone(), Value::Object(args)));
            resto = &resto[fin..];
        }
    }
    for n in nombres {
        let mut resto = texto;
        while let Some(i) = resto.find(&format!("{n}{{")) {
            let cuerpo_ini = i + n.len() + 1;
            let Some(fin) = resto[cuerpo_ini..].find('}') else { break };
            let cuerpo = resto[cuerpo_ini..cuerpo_ini + fin].replace("<|\"|>", "\"");
            let mut args = serde_json::Map::new();
            for par in cuerpo.split(',') {
                if let Some((k, v)) = par.split_once(':') {
                    let v = v.trim().trim_matches('"');
                    let valor = v.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(v));
                    args.insert(k.trim().trim_matches('"').to_string(), valor);
                }
            }
            out.push((String::new(), n.clone(), Value::Object(args)));
            resto = &resto[cuerpo_ini + fin..];
        }
    }
    out
}

fn args_cortos(a: &Value) -> String {
    let s = match a.as_object() {
        Some(o) => o.values().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect::<Vec<_>>().join(", "),
        None => String::new(),
    };
    if s.chars().count() > 60 { format!("{}…", s.chars().take(60).collect::<String>()) } else { s }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn abrelo() {
        assert!(es_abrelo("Ábrelo."));
        assert!(es_abrelo("ábrela por favor"));
        assert!(!es_abrelo("abre la calculadora"));
    }

    #[test]
    fn enrutamiento_iurefficient() {
        assert!(es_de_iurefficient("¿qué tareas tengo pendientes?"));
        assert!(es_de_iurefficient("¿cómo va el expediente de Heralva?"));
        assert!(es_de_iurefficient("¿qué vence esta semana?"));
        assert!(!es_de_iurefficient("¿qué archivos hay en el expediente de Heralva?"));
        assert!(es_de_la_suite("¿qué archivos hay en el expediente de Heralva?"));
        assert!(!es_de_iurefficient("abre la calculadora"));
        // «mi día» (F2)
        assert!(es_de_iurefficient("¿qué tengo hoy?"));
        assert!(es_de_iurefficient("¿cuántas horas llevo esta semana?"));
        assert!(es_de_iurefficient("¿tengo notificaciones nuevas?"));
        assert!(es_de_iurefficient("¿qué reuniones tengo mañana?"));
        assert!(es_de_iurefficient("busca al cliente Heralva"));
        assert!(!es_de_iurefficient("graba la junta con Pérez"));
        assert!(es_de_la_suite("graba la junta con Pérez"));
        assert!(!es_de_iurefficient("¿qué tengo en Descargas?"));
        // F4
        assert!(es_de_iurefficient("registra hora y media en Juárez por revisión de pruebas"));
        assert!(es_de_iurefficient("crea una tarea para mañana: llamar al cliente Pérez"));
        assert!(es_de_iurefficient("agéndame una reunión el jueves a las 10"));
        assert!(es_de_iurefficient("márcalas como leídas"));
    }

    #[test]
    fn tareas_para_claude() {
        assert!(es_tarea_de_sistema("crea un script que respalde mis documentos cada noche"));
        assert!(es_tarea_de_sistema("instala gimp"));
        assert!(es_tarea_de_sistema("renombra las fotos de descargas con la fecha"));
        assert!(es_tarea_de_sistema("¿cuánto espacio en disco me queda?"));
        assert!(!es_tarea_de_sistema("abre la calculadora"));
        assert!(!es_tarea_de_sistema("¿qué tengo hoy?"));
        assert!(!es_tarea_de_sistema("¿quién fue Porfirio Díaz?"));
        assert!(!es_tarea_de_sistema("sube el volumen"));
        assert!(!es_tarea_de_sistema("¿Conoces el código de Hammurabi?"));
    }

    #[test]
    fn abrir_apps_y_web() {
        assert_eq!(app_a_abrir("Abre Google Chrome").as_deref(), Some("google chrome"));
        assert_eq!(app_a_abrir("inicia la calculadora.").as_deref(), Some("la calculadora"));
        assert_eq!(app_a_abrir("Abre la página del New York Times y leme los encabezados"), None);
        assert_eq!(app_a_abrir("abre el editor y vamos a escribir"), None);
        assert!(!es_de_la_suite("Abre la página del New York Times y leme los encabezados"));
        assert!(pide_web("Abre la página del New York Times y leme los encabezados"));
        assert_eq!(app_a_abrir("abre el navegador").as_deref(), Some("el navegador"));
    }

    /// Con el LLM configurado (solo clasifica, no ejecuta nada): `cargo test -- --ignored enrutador_real`.
    #[test]
    #[ignore]
    fn enrutador_real() {
        let cfg = crate::config::Config::cargar();
        let ag = Agente::new(cfg.comandos.llm.clone(), None, None);
        for (frase, esperado) in [
            ("transcribe la siguiente junta", Destino::Apps),
            ("graba la audiencia con Pérez", Destino::Apps),
            ("¿qué juntas tengo mañana?", Destino::Iurefficient),
            ("¿cuáles son las oportunidades abiertas?", Destino::Iurefficient),
            ("busca el contrato de Heralva en la unidad", Destino::Apps),
            ("crea un script que respalde mis documentos", Destino::Sistema),
            ("abre el New York Times", Destino::Escritorio),
            ("¿quién fue Benito Juárez?", Destino::Conversacion),
            ("registra dos horas en Heralva", Destino::Iurefficient),
        ] {
            let d = ag.enrutar(frase, &[]).expect("enrutar");
            println!("{frase} → {d:?}");
            assert_eq!(d, esperado, "{frase}");
        }
    }

    #[test]
    fn nombres_de_la_suite() {
        assert_eq!(normalizar_nombres("las oportunidades abiertas en Euroficient"), "las oportunidades abiertas en Iurefficient");
        assert_eq!(normalizar_nombres("abre Yurefficient."), "abre Iurefficient.");
        assert_eq!(normalizar_nombres("los archivos de iuredab"), "los archivos de IureDav");
        assert_eq!(normalizar_nombres("eficiente y suficiente"), "eficiente y suficiente");
        assert!(es_de_iurefficient("Dime cuáles son las oportunidades abiertas"));
        assert!(!es_de_la_suite("Dime cuáles son las oportunidades abiertas"));
        assert!(es_de_la_suite("¿qué hay en la unidad de Juárez?"));
    }

    #[test]
    fn dictado_por_voz() {
        assert!(es_dictar("te voy a dictar"));
        assert!(es_dictar("abre un editor de texto y vamos a escribir unas cosas"));
        assert!(pide_abrir("abre un editor de texto y vamos a escribir unas cosas"));
        assert!(!pide_abrir("te voy a dictar"));
        assert!(es_fin_dictado("termina el dictado"));
        assert!(es_fin_dictado("Detener dictado"));
        assert!(es_fin_dictado("Terminar dictado"));
        assert!(es_fin_dictado("deja de dictar"));
        assert!(es_fin_dictado("Termina Victila"));
        assert!(!es_fin_dictado("termina la frase con un punto"));
        assert!(!es_fin_dictado("se terminó el día"));
        assert!(!es_dictar("termina el dictado"));
        assert!(!es_dictar("abre la calculadora"));
    }

    #[test]
    fn enrutamiento_por_palabras() {
        assert!(es_de_la_suite("busca el flujo de despacho de Heralva y dime cuántas páginas tiene"));
        assert!(es_de_la_suite("graba la junta con Pérez"));
        assert!(!es_de_la_suite("busca en internet quién fue Porfirio Díaz"));
        assert!(!es_de_la_suite("¿cuánto es el 15 % de 2300?"));
        assert!(pide_web("busca en google el clima"));
        assert!(!pide_web("busca el contrato de Juárez"));
    }

    #[test]
    fn llamada_escrita_como_texto() {
        let v = llamadas_en_texto("abrir_aplicacion{nombre:<|\"|>Excel<|\"|>}", &herramientas(false));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, "abrir_aplicacion");
        assert_eq!(v[0].2["nombre"], "Excel");
        let v = llamadas_en_texto("volumen{accion:\"subir\",cantidad:20}", &herramientas(false));
        assert_eq!(v[0].2["cantidad"], 20);
        assert!(llamadas_en_texto("Listo, ya abrí la calculadora.", &herramientas(false)).is_empty());
        let v = llamadas_en_texto("calcular(1585 * (0.23))", &herramientas(false));
        assert_eq!((v[0].1.as_str(), v[0].2["expresion"].as_str()), ("calcular", Some("1585 * (0.23)")));
        let v = llamadas_en_texto("hora_fecha()", &herramientas(false));
        assert_eq!(v[0].1, "hora_fecha");
        let v = llamadas_en_texto("abrir_url(url=\"es.wikipedia.org\")", &herramientas(false));
        assert_eq!(v[0].2["url"], "es.wikipedia.org");
    }
}
