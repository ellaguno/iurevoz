//! Orquestación: audio → ASR → puntuación dictada → (corrección LLM) → inyección.
//!
//! Tres hilos: el de ASR (latencia crítica, nunca espera a la red), el de salida, que
//! corrige e inyecta frase por frase en orden, y el del agente, que atiende instrucciones.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use sherpa_onnx::LinearResampler;

use crate::agente::Agente;
use crate::asr::{Reconocedor, SR, Salida};
use crate::control::{Control, Estado};
use crate::config::{Config, Inyeccion};
use crate::correccion::{Corrector, Resultado};
use crate::modelos;
use crate::portal::Portal;
use crate::puntuacion::Puntuador;
use crate::voz::Voz;

pub enum Orden {
    Audio { muestras: Vec<f32>, tasa: u32 },
    /// Fin del dictado: sale la última frase y se reinicia la puntuación.
    Fin,
    /// Nueva configuración (recarga el modelo si cambió).
    Config(Config),
    Descargar,
}

#[derive(Debug, Clone)]
pub enum Evento {
    Estado(String),
    Listo { modelo: String, carga_ms: u128 },
    FaltanModelos(Vec<String>),
    Descarga { pieza: String, fraccion: f32 },
    Parcial(String),
    Frase { id: usize, texto: String },
    Corregida { id: usize, res: Resultado },
    ErrorCorreccion { id: usize, msg: String },
    /// Instrucción recibida en modo comando.
    Comando { id: usize, instruccion: String },
    /// El modo comando espera la instrucción (se dijo solo el prefijo).
    EsperandoComando,
    Accion { id: usize, texto: String },
    Respuesta { id: usize, texto: String },
    ErrorComando { id: usize, msg: String },
    Error(String),
}

pub struct Motor {
    pub tx: Sender<Orden>,
    pub eventos: Receiver<Evento>,
}

type Notificar = Arc<dyn Fn() + Send + Sync>;

impl Motor {
    pub fn iniciar(
        cfg: Config,
        portal: Option<Portal>,
        voz: Option<Voz>,
        estado: Estado,
        control: Sender<Control>,
        notificar: Notificar,
    ) -> Self {
        let (tx, rx) = unbounded::<Orden>();
        let (tx_ev, eventos) = unbounded::<Evento>();
        let (tx_sal, rx_sal) = unbounded::<(usize, String, Config)>();
        let (tx_ag, rx_ag) = unbounded::<(usize, String, Config)>();

        let ev = Emisor { tx: tx_ev.clone(), notificar: notificar.clone() };
        let rutas = Rutas { salida: tx_sal, agente: tx_ag, estado, control };
        std::thread::Builder::new()
            .name("asr".into())
            .spawn(move || hilo_asr(cfg, rx, rutas, ev))
            .expect("hilo asr");

        let ev = Emisor { tx: tx_ev.clone(), notificar: notificar.clone() };
        let p = portal.clone();
        std::thread::Builder::new()
            .name("salida".into())
            .spawn(move || hilo_salida(rx_sal, p, ev))
            .expect("hilo salida");

        let ev = Emisor { tx: tx_ev, notificar };
        std::thread::Builder::new()
            .name("agente".into())
            .spawn(move || hilo_agente(rx_ag, portal, voz, ev))
            .expect("hilo agente");

        Self { tx, eventos }
    }
}

#[derive(Clone)]
struct Emisor {
    tx: Sender<Evento>,
    notificar: Notificar,
}

impl Emisor {
    fn enviar(&self, e: Evento) {
        if std::env::var_os("IUREVOZ_LOG").is_some() && !matches!(e, Evento::Parcial(_) | Evento::Descarga { .. }) {
            eprintln!("[iurevoz] {e:?}");
        }
        let _ = self.tx.send(e);
        (self.notificar)();
    }
}

/// A dónde va cada frase final.
struct Rutas {
    salida: Sender<(usize, String, Config)>,
    agente: Sender<(usize, String, Config)>,
    estado: Estado,
    control: Sender<Control>,
}

/// Si la frase empieza con el prefijo («computadora, abre…»), devuelve el resto. Tolera
/// un artículo o «oye» delante y variantes que comparten raíz («el computador», «computadoras»).
/// `tolerante` solo en modo comando: al dictar, «la computadora está lenta» es texto.
fn quitar_prefijo(frase: &str, prefijo: &str, tolerante: bool) -> Option<String> {
    let norm = |s: &str| crate::correccion::palabras(s);
    let p = norm(prefijo);
    let mut f = norm(frase);
    let mut saltadas = 0;
    while tolerante && f.len() > p.len() && matches!(f[0].as_str(), "el" | "la" | "oye" | "hey" | "ok") && p.first().is_some_and(|x| *x != f[0]) {
        f.remove(0);
        saltadas += 1;
    }
    let parecida = |a: &str, b: &str| a == b || (tolerante && b.chars().count() >= 8 && a.chars().take(8).eq(b.chars().take(8)));
    if p.is_empty() || f.len() < p.len() || !f.iter().zip(&p).all(|(a, b)| parecida(a, b)) {
        return None;
    }
    // se corta en el texto original tras tantas palabras como tenga el prefijo
    let mut resto = frase;
    for _ in 0..p.len() + saltadas {
        resto = resto.trim_start_matches(|c: char| !c.is_alphanumeric());
        resto = resto.trim_start_matches(char::is_alphanumeric);
    }
    Some(resto.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string())
}

fn hilo_asr(mut cfg: Config, rx: Receiver<Orden>, rutas: Rutas, ev: Emisor) {
    let mut rec = cargar(&cfg, &ev);
    let mut punt = Puntuador::new();
    let mut remuestreo: Option<(u32, LinearResampler)> = None;
    let mut id = 0usize;
    // IUREVOZ_GRABAR=1 guarda el audio de cada dictado (solo local) para depurar el ASR
    let grabar = std::env::var_os("IUREVOZ_GRABAR").is_some();
    let mut grabacion: Vec<f32> = Vec::new();

    let mut emitir = |salidas: Vec<Salida>, cfg: &Config, punt: &mut Puntuador| {
        for s in salidas {
            match s {
                Salida::Parcial(t) => ev.enviar(Evento::Parcial(t)),
                Salida::Final(t) => {
                    if std::env::var_os("IUREVOZ_LOG").is_some() {
                        eprintln!("[iurevoz] asr: {t:?}");
                    }
                    // ¿instrucción en vez de dictado?
                    let instruccion = if rutas.estado.modo_comando.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        let _ = rutas.control.send(Control::FinComando);
                        // también se quita el prefijo si se dijo por costumbre
                        Some(quitar_prefijo(&t, &cfg.comandos.prefijo, true).filter(|r| !r.is_empty()).unwrap_or_else(|| t.clone()))
                    } else if cfg.comandos.activos {
                        quitar_prefijo(&t, &cfg.comandos.prefijo, false)
                    } else {
                        None
                    };
                    if let Some(i) = instruccion {
                        if i.is_empty() {
                            // solo se dijo el prefijo: la siguiente frase es la instrucción
                            rutas.estado.modo_comando.store(true, std::sync::atomic::Ordering::Relaxed);
                            ev.enviar(Evento::EsperandoComando);
                        } else {
                            id += 1;
                            ev.enviar(Evento::Comando { id, instruccion: i.clone() });
                            let _ = rutas.agente.send((id, i, cfg.clone()));
                        }
                        continue;
                    }
                    let texto = punt.procesar(&t, cfg.puntuacion_dictada);
                    // «nuevo párrafo» solo produce saltos de línea: no se descarta
                    if texto.is_empty() {
                        continue;
                    }
                    id += 1;
                    ev.enviar(Evento::Frase { id, texto: texto.clone() });
                    let _ = rutas.salida.send((id, texto, cfg.clone()));
                }
            }
        }
    };

    for orden in rx {
        match orden {
            Orden::Audio { muestras, tasa } => {
                let Some(r) = rec.as_mut() else { continue };
                let m16 = if tasa as i32 == SR {
                    muestras
                } else {
                    if remuestreo.as_ref().is_none_or(|(t, _)| *t != tasa) {
                        remuestreo = LinearResampler::create(tasa as i32, SR).map(|r| (tasa, r));
                    }
                    match &remuestreo {
                        Some((_, rs)) => rs.resample(&muestras, false),
                        None => continue,
                    }
                };
                if grabar {
                    grabacion.extend_from_slice(&m16);
                }
                let s = r.alimentar(&m16);
                emitir(s, &cfg, &mut punt);
            }
            Orden::Fin => {
                if grabar && !grabacion.is_empty() {
                    guardar_grabacion(&grabacion, &ev);
                    grabacion.clear();
                }
                if let Some(r) = rec.as_mut() {
                    let s = r.terminar();
                    emitir(s, &cfg, &mut punt);
                }
                punt.reiniciar();
                if let Some((_, rs)) = &remuestreo {
                    rs.reset();
                }
            }
            Orden::Config(nueva) => {
                let recargar = nueva.modelo_asr != cfg.modelo_asr || nueva.hilos != cfg.hilos || nueva.silencio != cfg.silencio;
                cfg = nueva;
                if recargar || rec.is_none() {
                    drop(rec.take()); // libera la RAM del modelo anterior antes de cargar el nuevo
                    rec = cargar(&cfg, &ev);
                }
            }
            Orden::Descargar => {
                let ev2 = ev.clone();
                let r = modelos::descargar(cfg.modelo_asr, |pieza, fraccion| {
                    ev2.enviar(Evento::Descarga { pieza: pieza.to_string(), fraccion })
                });
                match r {
                    Ok(()) => rec = cargar(&cfg, &ev),
                    Err(e) => ev.enviar(Evento::Error(format!("descarga: {e:#}"))),
                }
            }
        }
    }
}

fn guardar_grabacion(muestras: &[f32], ev: &Emisor) {
    let dir = crate::config::dir_datos().join("depuracion");
    let _ = std::fs::create_dir_all(&dir);
    let seg = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let ruta = dir.join(format!("dictado-{seg}.wav"));
    if sherpa_onnx::write(&ruta.to_string_lossy(), muestras, SR) {
        ev.enviar(Evento::Estado(format!("Audio guardado en {}", ruta.display())));
    }
}

fn cargar(cfg: &Config, ev: &Emisor) -> Option<Reconocedor> {
    let falta = modelos::faltantes(cfg.modelo_asr);
    if !falta.is_empty() {
        ev.enviar(Evento::FaltanModelos(falta.iter().map(|s| s.to_string()).collect()));
        return None;
    }
    ev.enviar(Evento::Estado(format!("Cargando {:?}…", cfg.modelo_asr)));
    let t = std::time::Instant::now();
    match Reconocedor::cargar(cfg) {
        Ok(r) => {
            ev.enviar(Evento::Listo { modelo: format!("{:?}", cfg.modelo_asr), carga_ms: t.elapsed().as_millis() });
            Some(r)
        }
        Err(e) => {
            ev.enviar(Evento::Error(format!("{e:#}")));
            None
        }
    }
}

fn hilo_agente(rx: Receiver<(usize, String, Config)>, portal: Option<Portal>, voz: Option<Voz>, ev: Emisor) {
    for (id, instruccion, cfg) in rx {
        let voz_resp = voz.clone().filter(|_| cfg.voz.responder);
        let ag = Agente::new(cfg.comandos.llm.clone(), portal.clone(), voz.clone());
        let r = ag.ejecutar(&instruccion, |accion| ev.enviar(Evento::Accion { id, texto: accion.to_string() }));
        match r {
            Ok(inf) => {
                if let Some(v) = &voz_resp {
                    if !inf.respuesta.is_empty() {
                        v.decir(inf.respuesta.clone());
                    }
                }
                ev.enviar(Evento::Respuesta { id, texto: inf.respuesta });
            }
            Err(e) => {
                if let Some(v) = &voz_resp {
                    v.decir("No pude hacerlo.");
                }
                ev.enviar(Evento::ErrorComando { id, msg: format!("{e:#}") });
            }
        }
    }
}

fn hilo_salida(rx: Receiver<(usize, String, Config)>, portal: Option<Portal>, ev: Emisor) {
    let mut corrector: Option<(crate::config::Correccion, Corrector)> = None;
    // lo ya entregado, como contexto para la siguiente corrección
    let mut contexto = String::new();
    for (id, texto, cfg) in rx {
        let mut final_ = texto.clone();
        if cfg.correccion.activa {
            let c = &cfg.correccion;
            let reusar = corrector.as_ref().is_some_and(|(prev, _)| {
                prev.servicio == c.servicio && prev.url == c.url && prev.clave == c.clave && prev.modelo == c.modelo && prev.glosario == c.glosario && prev.max_cambio == c.max_cambio
            });
            if !reusar {
                corrector = Some((c.clone(), Corrector::new(c.clone())));
            }
            let (_, corr) = corrector.as_ref().unwrap();
            // se corrige sin el espacio o salto inicial, que se repone después
            let inicio = texto.len() - texto.trim_start().len();
            let (prefijo, cuerpo) = texto.split_at(inicio);
            match corr.corregir(cuerpo, &contexto) {
                Ok(res) => {
                    final_ = format!("{prefijo}{}", res.texto);
                    ev.enviar(Evento::Corregida { id, res });
                }
                Err(e) => ev.enviar(Evento::ErrorCorreccion { id, msg: format!("{e:#}") }),
            }
        }
        contexto.push_str(&final_);
        if contexto.len() > 2000 {
            let mut corte = contexto.len() - 1000;
            while !contexto.is_char_boundary(corte) {
                corte += 1;
            }
            contexto.drain(..corte);
        }
        if cfg.inyeccion == Inyeccion::Teclear {
            if let Some(p) = &portal {
                p.escribir(final_);
            }
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn prefijo() {
        // al dictar: estricto
        assert_eq!(quitar_prefijo("Computadora, abre la calculadora", "computadora", false).as_deref(), Some("abre la calculadora"));
        assert_eq!(quitar_prefijo("computadora", "computadora", false).as_deref(), Some(""));
        assert_eq!(quitar_prefijo("La computadora está lenta", "computadora", false), None);
        assert_eq!(quitar_prefijo("Oye Iure, ¿qué hora es?", "oye iure", false).as_deref(), Some("qué hora es?"));
        // en modo comando: tolera artículo y variantes
        assert_eq!(quitar_prefijo("El computador sube el volumen", "computadora", true).as_deref(), Some("sube el volumen"));
        assert_eq!(quitar_prefijo("Mi computadora está lenta", "computadora", true), None);
        assert_eq!(quitar_prefijo("Computación es una carrera", "computadora", true), None);
    }
}
