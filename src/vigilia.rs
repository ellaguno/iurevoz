//! Modo «siempre escuchando»: espera una palabra clave («oye compu») gastando lo mínimo.
//!
//! Primer filtro: VAD Silero (≈0.5 % de un núcleo). Solo cuando alguien habla se revisan
//! los primeros ~1.8 s de la frase; si empiezan con la palabra clave, se despierta el modo
//! comando y ese mismo audio pasa al reconocedor principal. Si no, se descarta.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::asr::SR;

/// Audio previo al inicio de la voz que se conserva (el VAD tarda un poco en reaccionar).
const PREVIO: usize = SR as usize * 4 / 10;
/// Con esta cantidad de voz ya se decide (la palabra clave dura menos de un segundo).
const REVISAR_CON: usize = SR as usize * 13 / 10;
const VENTANA: usize = 512;

/// Cada cuánto se evalúa el detector dedicado mientras hay voz.
const PASO_DETECTOR: usize = SR as usize / 10;
/// Tras terminar la voz se sigue evaluando un poco (la palabra pudo acabar justo ahí).
const COLA: usize = SR as usize * 6 / 10;
/// Audio que se pasa al reconocedor al despertar con el detector.
const MAX_AL_DESPERTAR: usize = SR as usize * 3;

pub struct Vigilia {
    vad: VoiceActivityDetector,
    detector: Option<crate::detector::Detector>,
    /// últimos 1.2 s de audio (lo que ve el detector)
    anillo: VecDeque<f32>,
    desde_eval: usize,
    cola: usize,
    seguidas: u8,
    resto: Vec<f32>,
    previo: VecDeque<f32>,
    voz: Vec<f32>,
    hablando: bool,
    revisado: bool,
}

impl Vigilia {
    pub fn new(modelo_vad: &str) -> Result<Self> {
        let vc = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(modelo_vad.to_string()),
                threshold: 0.5,
                min_silence_duration: 0.3,
                min_speech_duration: 0.15,
                window_size: VENTANA as i32,
                max_speech_duration: 20.0,
            },
            sample_rate: SR,
            num_threads: 1,
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&vc, 30.0).context("no pude cargar el VAD de vigilia")?;
        Ok(Self {
            vad,
            detector: None,
            anillo: VecDeque::with_capacity(crate::detector::VENTANA),
            desde_eval: 0,
            cola: 0,
            seguidas: 0,
            resto: Vec::new(),
            previo: VecDeque::with_capacity(PREVIO),
            voz: Vec::new(),
            hablando: false,
            revisado: false,
        })
    }

    /// Usa el detector dedicado de esta palabra si está entrenado (si no, Moonshine).
    pub fn cargar_detector(&mut self, palabra: &str) -> bool {
        self.detector = crate::detector::Detector::cargar(palabra).ok();
        self.detector.is_some()
    }

    /// Evalúa el detector dedicado; devuelve true si despierta.
    fn evaluar(&mut self, n: usize) -> bool {
        let Some(d) = &self.detector else { return false };
        self.desde_eval += n;
        if self.desde_eval < PASO_DETECTOR || self.anillo.len() < crate::detector::VENTANA {
            return false;
        }
        self.desde_eval = 0;
        let (a, b) = self.anillo.as_slices();
        let ventana: Vec<f32> = a.iter().chain(b).copied().collect();
        let p = d.puntuar(&ventana).unwrap_or(0.0);
        if std::env::var_os("IUREVOZ_LOG_DETECTOR").is_some() {
            eprintln!("[iurevoz] detector: {p:.3}");
        }
        // dos ventanas seguidas sobre el umbral: menos falsas activaciones
        self.seguidas = if p >= d.meta.umbral { self.seguidas + 1 } else { 0 };
        self.seguidas >= 2
    }

    /// Alimenta audio a 16 kHz. `revisar` transcribe un tramo corto y dice si empieza con
    /// la palabra clave. Devuelve el audio de la frase (desde un poco antes) al despertar.
    pub fn alimentar(&mut self, muestras: &[f32], mut revisar: impl FnMut(&[f32]) -> bool) -> Option<Vec<f32>> {
        self.resto.extend_from_slice(muestras);
        let mut i = 0;
        let mut despierta = None;
        while i + VENTANA <= self.resto.len() {
            let bloque: Vec<f32> = self.resto[i..i + VENTANA].to_vec();
            i += VENTANA;
            self.vad.accept_waveform(&bloque);
            // el VAD guarda sus propios segmentos: aquí solo interesa si hay voz ahora
            while self.vad.front().is_some() {
                self.vad.pop();
            }
            let hay_voz = self.vad.detected();
            if hay_voz && !self.hablando {
                self.hablando = true;
                self.revisado = false;
                self.voz.clear();
                self.voz.extend(self.previo.iter());
            }
            if self.hablando {
                self.voz.extend_from_slice(&bloque);
                let termina = !hay_voz;
                if self.detector.is_none() && !self.revisado && (self.voz.len() >= REVISAR_CON || (termina && self.voz.len() > PREVIO + SR as usize / 4)) {
                    self.revisado = true;
                    let tramo = &self.voz[..self.voz.len().min(REVISAR_CON + PREVIO)];
                    if revisar(tramo) {
                        despierta = Some(std::mem::take(&mut self.voz));
                        self.hablando = false;
                    }
                }
                if termina {
                    self.hablando = false;
                    self.voz.clear();
                }
                // frase larga: basta con los últimos segundos
                if self.voz.len() > MAX_AL_DESPERTAR * 2 {
                    self.voz.drain(..self.voz.len() - MAX_AL_DESPERTAR);
                }
            }
            for &x in &bloque {
                if self.previo.len() == PREVIO {
                    self.previo.pop_front();
                }
                self.previo.push_back(x);
                if self.anillo.len() == crate::detector::VENTANA {
                    self.anillo.pop_front();
                }
                self.anillo.push_back(x);
            }
            // detector dedicado: solo mientras hay voz y un poco después
            if self.detector.is_some() && despierta.is_none() {
                if hay_voz {
                    self.cola = COLA;
                } else {
                    self.cola = self.cola.saturating_sub(VENTANA);
                }
                if (hay_voz || self.cola > 0) && self.evaluar(VENTANA) {
                    let (a, b) = self.anillo.as_slices();
                    let mut audio: Vec<f32> = if self.voz.len() > self.anillo.len() { self.voz.clone() } else { a.iter().chain(b).copied().collect() };
                    if audio.len() > MAX_AL_DESPERTAR {
                        audio.drain(..audio.len() - MAX_AL_DESPERTAR);
                    }
                    despierta = Some(audio);
                    self.hablando = false;
                } else if !hay_voz && self.cola == 0 {
                    self.seguidas = 0;
                }
            }
            if despierta.is_some() {
                break;
            }
        }
        self.resto.drain(..i);
        if despierta.is_some() {
            // lo que quedó sin procesar también es de la instrucción
            let mut a = despierta.unwrap();
            a.append(&mut self.resto);
            self.reiniciar();
            return Some(a);
        }
        None
    }

    pub fn reiniciar(&mut self) {
        self.anillo.clear();
        self.seguidas = 0;
        self.cola = 0;
        self.desde_eval = 0;
        self.vad.reset();
        self.resto.clear();
        self.voz.clear();
        self.hablando = false;
        self.revisado = false;
    }
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

/// Si el texto empieza con algo parecido a la palabra clave, devuelve cuántas palabras
/// ocupa. Compara sin espacios ni signos, con una tolerancia (0 = exacto).
pub fn coincide(texto: &str, palabra: &str, tolerancia: f32) -> Option<usize> {
    let objetivo: Vec<char> = crate::correccion::palabras(palabra).concat().chars().collect();
    if objetivo.is_empty() {
        return None;
    }
    let p = crate::correccion::palabras(texto);
    // se prueban las primeras 1..=n+1 palabras: el ASR puede partir o juntar palabras
    let n = crate::correccion::palabras(palabra).len();
    (1..=(n + 1).min(p.len()))
        .map(|k| {
            let c: Vec<char> = p[..k].concat().chars().collect();
            (k, distancia(&c, &objetivo) as f32 / objetivo.len() as f32)
        })
        .filter(|(_, d)| *d <= tolerancia)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

/// Quita la palabra clave del inicio («Oye compu, abre…» → «abre…»).
pub fn quitar(texto: &str, palabra: &str, tolerancia: f32) -> Option<String> {
    let k = coincide(texto, palabra, tolerancia)?;
    let mut resto = texto;
    for _ in 0..k {
        resto = resto.trim_start_matches(|c: char| !c.is_alphanumeric());
        resto = resto.trim_start_matches(char::is_alphanumeric);
    }
    Some(resto.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn palabra_clave_tolerante() {
        let t = 0.34;
        assert_eq!(coincide("Oye compu, abre la calculadora", "oye compu", t), Some(2));
        assert_eq!(coincide("Oye, compo", "oye compu", t), Some(2));
        assert_eq!(coincide("Oyecompu qué hora es", "oye compu", t), Some(1));
        assert_eq!(coincide("Hoy compu", "oye compu", t), Some(2));
        assert_eq!(coincide("Hoy compré pan", "oye compu", t), None);
        assert_eq!(coincide("Oye, ¿vienes?", "oye compu", t), None);
        assert_eq!(coincide("La computadora", "oye compu", t), None);
        assert_eq!(quitar("Oye compu, abre la calculadora", "oye compu", t).as_deref(), Some("abre la calculadora"));
        assert_eq!(quitar("Oye compu", "oye compu", t).as_deref(), Some(""));
    }
}
