//! Detector dedicado de palabra clave: red convolucional pequeña sobre log-mel.
//! Se entrena en `entrenamiento` y aquí solo se carga y evalúa.

use std::path::PathBuf;

use anyhow::{Context, Result};
use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{Conv1d, Conv1dConfig, Linear, VarBuilder, VarMap, conv1d, linear};
use serde::{Deserialize, Serialize};

use crate::asr::SR;
use crate::rasgos::{BANDAS, LogMel, marcos_de};

/// Duración de la ventana que ve la red.
pub const VENTANA_S: f32 = 1.2;
pub const VENTANA: usize = (SR as f32 * VENTANA_S) as usize;

pub struct Red {
    c1: Conv1d,
    c2: Conv1d,
    c3: Conv1d,
    c4: Conv1d,
    salida: Linear,
}

impl Red {
    pub fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        let p = |stride| Conv1dConfig { padding: 1, stride, ..Default::default() };
        Ok(Self {
            c1: conv1d(BANDAS, 48, 3, p(1), vb.pp("c1"))?,
            c2: conv1d(48, 48, 3, p(2), vb.pp("c2"))?,
            c3: conv1d(48, 64, 3, p(2), vb.pp("c3"))?,
            c4: conv1d(64, 64, 3, p(2), vb.pp("c4"))?,
            salida: linear(64, 1, vb.pp("salida"))?,
        })
    }

    /// x: [lote, BANDAS, marcos] → logits [lote]
    pub fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let x = self.c1.forward(x)?.relu()?;
        let x = self.c2.forward(&x)?.relu()?;
        let x = self.c3.forward(&x)?.relu()?;
        let x = self.c4.forward(&x)?.relu()?;
        // máximo en el tiempo: la palabra puede estar en cualquier parte de la ventana
        let x = x.max(D::Minus1)?;
        self.salida.forward(&x)?.squeeze(1)
    }
}

/// Lo que se guarda junto a los pesos.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub palabra: String,
    pub umbral: f32,
    pub media: Vec<f32>,
    pub desv: Vec<f32>,
    pub recall: f32,
    pub falsas_por_hora: f32,
    pub fecha: String,
}

/// Archivos del detector de una palabra clave.
pub fn rutas(palabra: &str) -> (PathBuf, PathBuf) {
    let nombre: String = crate::correccion::palabras(palabra).join("-");
    let dir = crate::config::dir_datos().join("palabra-clave");
    (dir.join(format!("{nombre}.safetensors")), dir.join(format!("{nombre}.json")))
}

pub struct Detector {
    red: Red,
    pub meta: Meta,
    mel: LogMel,
    dev: Device,
}

impl Detector {
    pub fn cargar(palabra: &str) -> Result<Self> {
        let (pesos, meta) = rutas(palabra);
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(&meta).context("sin detector entrenado")?)?;
        let dev = Device::Cpu;
        let mut vm = VarMap::new();
        let red = Red::new(VarBuilder::from_varmap(&vm, DType::F32, &dev))?;
        vm.load(&pesos)?;
        Ok(Self { red, meta, mel: LogMel::new(), dev })
    }

    /// Normaliza marcos log-mel con la media y desviación del entrenamiento → [BANDAS, marcos].
    pub fn tensor(marcos: &[[f32; BANDAS]], media: &[f32], desv: &[f32]) -> Vec<f32> {
        let t = marcos.len();
        let mut v = vec![0f32; BANDAS * t];
        for (j, m) in marcos.iter().enumerate() {
            for b in 0..BANDAS {
                v[b * t + j] = (m[b] - media[b]) / desv[b];
            }
        }
        v
    }

    /// Probabilidad (0–1) de que la ventana termine con la palabra clave.
    pub fn puntuar(&self, ventana: &[f32]) -> Result<f32> {
        let marcos = self.mel.calcular(ventana);
        let t = marcos.len();
        if t != marcos_de(VENTANA) {
            return Ok(0.0);
        }
        let v = Self::tensor(&marcos, &self.meta.media, &self.meta.desv);
        let x = Tensor::from_vec(v, (1, BANDAS, t), &self.dev)?;
        let logit = self.red.forward(&x)?.to_vec1::<f32>()?[0];
        Ok(1.0 / (1.0 + (-logit).exp()))
    }
}
