//! Espectrograma log-mel (40 bandas, ventana de 25 ms cada 10 ms, 16 kHz) para el
//! detector de palabra clave.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub const BANDAS: usize = 40;
pub const SALTO: usize = 160; // 10 ms
const VENTANA: usize = 400; // 25 ms
const NFFT: usize = 512;

pub struct LogMel {
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    /// filtros triangulares: (primer bin, pesos)
    filtros: Vec<(usize, Vec<f32>)>,
}

fn hz_a_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_a_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

impl LogMel {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(NFFT);
        let hann = (0..VENTANA).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (VENTANA - 1) as f32).cos()).collect();
        let (lo, hi) = (hz_a_mel(60.0), hz_a_mel(7600.0));
        let puntos: Vec<f32> = (0..BANDAS + 2).map(|i| mel_a_hz(lo + (hi - lo) * i as f32 / (BANDAS + 1) as f32)).collect();
        let bin = |f: f32| f * NFFT as f32 / 16000.0;
        let filtros = (0..BANDAS)
            .map(|b| {
                let (a, c, d) = (bin(puntos[b]), bin(puntos[b + 1]), bin(puntos[b + 2]));
                let ini = a.floor() as usize;
                let fin = (d.ceil() as usize).min(NFFT / 2);
                let pesos = (ini..=fin)
                    .map(|k| {
                        let k = k as f32;
                        if k <= c { ((k - a) / (c - a)).max(0.0) } else { ((d - k) / (d - c)).max(0.0) }
                    })
                    .collect();
                (ini, pesos)
            })
            .collect();
        Self { fft, hann, filtros }
    }

    /// Marcos de `audio` (16 kHz): cada uno, 40 valores log-mel.
    pub fn calcular(&self, audio: &[f32]) -> Vec<[f32; BANDAS]> {
        if audio.len() < VENTANA {
            return Vec::new();
        }
        let n = (audio.len() - VENTANA) / SALTO + 1;
        let mut buf = vec![Complex::new(0.0f32, 0.0); NFFT];
        let mut out = Vec::with_capacity(n);
        for m in 0..n {
            let ini = m * SALTO;
            for (i, x) in buf.iter_mut().enumerate() {
                *x = if i < VENTANA { Complex::new(audio[ini + i] * self.hann[i], 0.0) } else { Complex::new(0.0, 0.0) };
            }
            self.fft.process(&mut buf);
            let mut marco = [0f32; BANDAS];
            for (b, (k0, pesos)) in self.filtros.iter().enumerate() {
                let e: f32 = pesos.iter().enumerate().map(|(j, w)| w * buf[k0 + j].norm_sqr()).sum();
                marco[b] = (e + 1e-6).ln();
            }
            out.push(marco);
        }
        out
    }
}

/// Cuántos marcos tiene un tramo de `muestras` (para fijar el tamaño de la ventana).
pub fn marcos_de(muestras: usize) -> usize {
    if muestras < VENTANA { 0 } else { (muestras - VENTANA) / SALTO + 1 }
}
