//! Orquestación: audio → ASR → puntuación dictada → (corrección LLM) → inyección.
//!
//! Dos hilos: el de ASR (latencia crítica, nunca espera a la red) y el de salida,
//! que corrige e inyecta frase por frase en orden.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use sherpa_onnx::LinearResampler;

use crate::asr::{Reconocedor, SR, Salida};
use crate::config::{Config, Inyeccion};
use crate::correccion::{Corrector, Resultado};
use crate::modelos;
use crate::portal::Portal;
use crate::puntuacion::Puntuador;

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
    Error(String),
}

pub struct Motor {
    pub tx: Sender<Orden>,
    pub eventos: Receiver<Evento>,
}

type Notificar = Arc<dyn Fn() + Send + Sync>;

impl Motor {
    pub fn iniciar(cfg: Config, portal: Option<Portal>, notificar: Notificar) -> Self {
        let (tx, rx) = unbounded::<Orden>();
        let (tx_ev, eventos) = unbounded::<Evento>();
        let (tx_sal, rx_sal) = unbounded::<(usize, String, Config)>();

        let ev = Emisor { tx: tx_ev.clone(), notificar: notificar.clone() };
        std::thread::Builder::new()
            .name("asr".into())
            .spawn(move || hilo_asr(cfg, rx, tx_sal, ev))
            .expect("hilo asr");

        let ev = Emisor { tx: tx_ev, notificar };
        std::thread::Builder::new()
            .name("salida".into())
            .spawn(move || hilo_salida(rx_sal, portal, ev))
            .expect("hilo salida");

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

fn hilo_asr(mut cfg: Config, rx: Receiver<Orden>, tx_sal: Sender<(usize, String, Config)>, ev: Emisor) {
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
                    let texto = punt.procesar(&t, cfg.puntuacion_dictada);
                    // «nuevo párrafo» solo produce saltos de línea: no se descarta
                    if texto.is_empty() {
                        continue;
                    }
                    id += 1;
                    ev.enviar(Evento::Frase { id, texto: texto.clone() });
                    let _ = tx_sal.send((id, texto, cfg.clone()));
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
