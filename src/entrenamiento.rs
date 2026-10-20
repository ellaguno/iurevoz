//! Entrena el detector dedicado de una palabra clave, todo local:
//!
//! 1. Positivos: la palabra dicha por varias voces sintéticas, a varias velocidades y con
//!    distinta puntuación (cambia la entonación), aumentados con ruido real del cuarto,
//!    volumen, eco y pequeños cambios de tono.
//! 2. Negativos: frases parecidas («oye compadre», cada palabra sola), frases comunes
//!    sintetizadas, pedazos de la palabra cortados a la mitad, ruido y, sobre todo, las
//!    grabaciones reales del usuario (`~/.local/share/iurevoz/depuracion`).
//! 3. Red convolucional pequeña (candle, CPU) y umbral calibrado con voces y grabaciones
//!    que no se usaron para entrenar.

use anyhow::{Context, Result, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::{AdamW, Optimizer, ParamsAdamW, VarBuilder, VarMap};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};

use crate::asr::SR;
use crate::detector::{Meta, Red, VENTANA, rutas};
use crate::rasgos::{BANDAS, LogMel, marcos_de};
use crate::voz;

/// Voces de entrenamiento y de validación (estas nunca se ven al entrenar).
const VOCES_ENTRENAR: &[&str] = &["piper-ald", "piper-claude", "piper-davefx", "piper-sharvard", "supertonic", "kokoro-dora"];
const VOCES_VALIDAR: &[&str] = &["piper-daniela", "piper-carlfm", "kokoro-alex"];
const VELOCIDADES: &[f32] = &[0.8, 0.9, 1.0, 1.1, 1.25];

/// Frases comunes como negativos (conversación, oficina, trabajo jurídico y técnico).
const FRASES: &[&str] = &[
    "Buenos días, ¿cómo amaneciste?", "Pásame el expediente del amparo, por favor.", "Hoy tenemos audiencia a las diez.",
    "¿Ya comiste algo?", "La reunión se movió para el jueves.", "No encuentro las llaves del coche.",
    "El servidor se reinició anoche.", "Abre la ventana, hace mucho calor.", "¿Me puedes llamar más tarde?",
    "Vamos a revisar el contrato de arrendamiento.", "Mañana voy al banco temprano.", "Qué buena está la comida.",
    "Necesito imprimir estos documentos.", "El cliente pidió una prórroga.", "Ya mandé el correo con los anexos.",
    "Mi hermano llega el sábado.", "Hay mucho tráfico en el periférico.", "¿Dónde dejaste el cargador?",
    "La sentencia salió favorable.", "Hay que actualizar el sistema operativo.", "Compramos pan dulce para la merienda.",
    "Oye, ¿viste el partido de ayer?", "Oye, ¿me pasas la sal?", "Oye, espérame tantito.", "Hoy compré fruta en el mercado.",
    "Con pura paciencia se logra.", "Esa computadora ya está muy vieja.", "El compuesto químico es estable.",
    "Compadre, ¿cómo te ha ido?", "Hoy comí en casa de mi mamá.", "Joven, ¿me cobra por favor?", "Oye compadre, ven acá.",
    "Comprueba que el archivo esté guardado.", "La compu de la oficina no prende.", "Oigan todos, empezamos en cinco minutos.",
    "Voy a preparar café.", "Se cayó el internet otra vez.", "Firma aquí y aquí.", "¿Qué hora es en Madrid?",
    "El perro ladró toda la noche.", "Ya casi termino el informe.", "Gracias por tu ayuda.", "Nos vemos la próxima semana.",
];

const TEXTOS_POSITIVOS: &[&str] = &["{}", "{}.", "{},", "¡{}!", "¿{}?", "{}..."];

fn log_paso(progreso: &dyn Fn(String), m: String) {
    if std::env::var_os("IUREVOZ_LOG").is_some() {
        eprintln!("[iurevoz] entrenamiento: {m}");
    }
    progreso(m);
}

/// Quita silencio al principio y al final.
fn recortar(a: &[f32]) -> Vec<f32> {
    let umbral = 0.01 * a.iter().fold(0f32, |m, x| m.max(x.abs()));
    let ini = a.iter().position(|x| x.abs() > umbral).unwrap_or(0);
    let fin = a.iter().rposition(|x| x.abs() > umbral).map_or(a.len(), |i| i + 1);
    a[ini.saturating_sub(SR as usize / 50)..(fin + SR as usize / 50).min(a.len())].to_vec()
}

fn a_16k(a: &[f32], sr: i32) -> Vec<f32> {
    if sr == SR {
        return a.to_vec();
    }
    sherpa_onnx::LinearResampler::create(sr, SR).map_or_else(Vec::new, |r| r.resample(a, true))
}

fn rms(a: &[f32]) -> f32 {
    (a.iter().map(|x| x * x).sum::<f32>() / a.len().max(1) as f32).sqrt()
}

/// Sintetiza `textos` con una voz (todos sus hablantes, hasta 3) y velocidades.
fn sintetizar(clave: &str, textos: &[String], velocidades: &[f32]) -> Result<Vec<Vec<f32>>> {
    if voz::falta(clave).is_some() {
        voz::descargar(clave, |_| {})?;
    }
    let s = voz::Sintetizador::cargar(clave, 1.0)?;
    // Kokoro tiene 54 hablantes de muchos idiomas: se usa solo el de la clave
    let hablantes = if clave.starts_with("kokoro") { 1 } else { s.hablantes().min(3) };
    let mut v = Vec::new();
    for h in 0..hablantes {
        for t in textos {
            for &vel in velocidades {
                let (m, sr) = s.generar_con(t, h, vel)?;
                let m = recortar(&a_16k(&m, sr));
                if !m.is_empty() {
                    v.push(m);
                }
            }
        }
    }
    Ok(v)
}

struct Aumento<'a> {
    rng: SmallRng,
    ruidos: &'a [Vec<f32>],
}

impl Aumento<'_> {
    /// Fondo de la ventana: ruido real del cuarto (o blanco) a volumen aleatorio.
    fn fondo(&mut self) -> Vec<f32> {
        let mut v = vec![0f32; VENTANA];
        let nivel = 10f32.powf(self.rng.random_range(-4.0..-1.6));
        if !self.ruidos.is_empty() && self.rng.random_bool(0.8) {
            let r = &self.ruidos[self.rng.random_range(0..self.ruidos.len())];
            let escala = nivel / rms(r).max(1e-5);
            for (i, x) in v.iter_mut().enumerate() {
                *x = r[i % r.len()] * escala;
            }
        } else {
            for x in v.iter_mut() {
                *x = nivel * (self.rng.random::<f32>() * 2.0 - 1.0);
            }
        }
        v
    }

    /// Variación de un clip: tono/tempo (±8 %), volumen y eco.
    fn variar(&mut self, clip: &[f32]) -> Vec<f32> {
        let f: f32 = self.rng.random_range(0.92..1.08);
        let mut c = sherpa_onnx::LinearResampler::create(SR, (SR as f32 / f) as i32).map_or_else(|| clip.to_vec(), |r| r.resample(clip, true));
        let g = 10f32.powf(self.rng.random_range(-0.8..0.2)) * 0.5 / c.iter().fold(1e-5f32, |m, x| m.max(x.abs()));
        c.iter_mut().for_each(|x| *x *= g);
        if self.rng.random_bool(0.5) {
            // eco simple: realimentación con retardo de 20–60 ms
            let d = self.rng.random_range(SR as usize / 50..SR as usize * 6 / 100);
            let k: f32 = self.rng.random_range(0.15..0.45);
            for i in d..c.len() {
                c[i] += k * c[i - d];
            }
        }
        c
    }

    /// Pone `clip` en una ventana con fondo; termina a `fin` muestras del inicio.
    fn colocar(&mut self, clip: &[f32], fin: usize) -> Vec<f32> {
        let mut v = self.fondo();
        let ini = fin.saturating_sub(clip.len());
        let desde = clip.len().saturating_sub(fin);
        for (i, x) in clip[desde..].iter().enumerate() {
            if ini + i < VENTANA {
                v[ini + i] += x;
            }
        }
        v
    }

    /// Fin aleatorio en los últimos 450 ms: así dispara cuando la palabra acaba de decirse.
    fn fin_positivo(&mut self) -> usize {
        VENTANA - self.rng.random_range(SR as usize / 20..SR as usize * 45 / 100)
    }
}

/// Grabaciones reales del usuario: ventanas de voz (negativos) y tramos de ruido.
fn grabaciones(validar_cada: usize) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let dir = crate::config::dir_datos().join("depuracion");
    let mut archivos: Vec<_> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    archivos.sort();
    let (mut ent, mut val, mut ruido) = (Vec::new(), Vec::new(), Vec::new());
    for (n, a) in archivos.iter().enumerate() {
        let Some(w) = sherpa_onnx::Wave::read(&a.to_string_lossy()) else { continue };
        let m = a_16k(w.samples(), w.sample_rate());
        let destino = if n % validar_cada == validar_cada - 1 { &mut val } else { &mut ent };
        let paso = SR as usize / 2;
        let mut i = 0;
        while i + VENTANA <= m.len() {
            let v = m[i..i + VENTANA].to_vec();
            if rms(&v) < 0.004 {
                if ruido.len() < 400 {
                    ruido.push(v);
                }
            } else {
                destino.push(v);
            }
            i += paso;
        }
    }
    (ent, val, ruido)
}

/// Ventanas de un audio largo (frases negativas sintetizadas).
fn ventanas_de(a: &[f32], paso: usize) -> Vec<Vec<f32>> {
    if a.len() < VENTANA {
        let mut v = vec![0f32; VENTANA];
        v[VENTANA - a.len()..].copy_from_slice(a);
        return vec![v];
    }
    (0..=(a.len() - VENTANA) / paso).map(|k| a[k * paso..k * paso + VENTANA].to_vec()).collect()
}

/// Palabras parecidas generadas a partir de la palabra clave (sirve para cualquier frase).
fn cercanas(palabra: &str) -> Vec<String> {
    let ps = crate::correccion::palabras(palabra);
    let mut v: Vec<String> = ps.iter().map(|p| p.to_string()).collect();
    if let Some(primera) = ps.first() {
        for otra in ["compadre", "cómo estás", "ven", "Lupe", "mira", "espera"] {
            v.push(format!("{primera} {otra}"));
        }
    }
    if let Some(ultima) = ps.last() {
        for otra in ["hoy", "la", "mi", "con"] {
            v.push(format!("{otra} {ultima}"));
        }
    }
    v.extend(["hoy compré", "computadora", "compuesto", "con pulso", "hoy comí", "joven", "oiga", "compañero"].map(String::from));
    v.retain(|c| crate::correccion::palabras(c) != ps);
    v
}

pub struct Datos {
    x: Vec<f32>,
    y: Vec<f32>,
    n: usize,
    marcos: usize,
}

impl Datos {
    fn guardar(&self, ruta: &std::path::Path) -> Result<()> {
        let mut b = Vec::with_capacity(16 + 4 * (self.x.len() + self.y.len()));
        b.extend((self.n as u64).to_le_bytes());
        b.extend((self.marcos as u64).to_le_bytes());
        b.extend(self.y.iter().flat_map(|v| v.to_le_bytes()));
        b.extend(self.x.iter().flat_map(|v| v.to_le_bytes()));
        std::fs::write(ruta, b)?;
        Ok(())
    }

    fn leer(ruta: &std::path::Path) -> Result<Self> {
        let b = std::fs::read(ruta)?;
        let n = u64::from_le_bytes(b[0..8].try_into()?) as usize;
        let marcos = u64::from_le_bytes(b[8..16].try_into()?) as usize;
        let f = |i: usize| f32::from_le_bytes(b[16 + 4 * i..20 + 4 * i].try_into().unwrap());
        let y = (0..n).map(f).collect();
        let x = (n..n + n * BANDAS * marcos).map(f).collect();
        Ok(Self { x, y, n, marcos })
    }
}

fn a_rasgos(mel: &LogMel, ventanas: &[(Vec<f32>, f32)]) -> (Vec<Vec<[f32; BANDAS]>>, Vec<f32>) {
    let mut r = Vec::with_capacity(ventanas.len());
    let mut y = Vec::with_capacity(ventanas.len());
    for (v, etiqueta) in ventanas {
        r.push(mel.calcular(v));
        y.push(*etiqueta);
    }
    (r, y)
}

fn normalizar(rasgos: &[Vec<[f32; BANDAS]>], media: &[f32], desv: &[f32], y: Vec<f32>) -> Datos {
    let marcos = marcos_de(VENTANA);
    let mut x = Vec::with_capacity(rasgos.len() * BANDAS * marcos);
    for r in rasgos {
        x.extend(crate::detector::Detector::tensor(r, media, desv));
    }
    Datos { n: rasgos.len(), x, y, marcos }
}

fn puntuar(red: &Red, d: &Datos, dev: &Device) -> Result<Vec<f32>> {
    let tam = BANDAS * d.marcos;
    let mut out = Vec::with_capacity(d.n);
    for ini in (0..d.n).step_by(256) {
        let fin = (ini + 256).min(d.n);
        let x = Tensor::from_slice(&d.x[ini * tam..fin * tam], (fin - ini, BANDAS, d.marcos), dev)?;
        let p = candle_nn::ops::sigmoid(&red.forward(&x)?)?.to_vec1::<f32>()?;
        out.extend(p);
    }
    Ok(out)
}

/// Entrena y guarda el detector de `palabra`. `progreso` recibe mensajes para la interfaz.
pub fn entrenar(palabra: &str, progreso: &dyn Fn(String)) -> Result<Meta> {
    let palabra = palabra.trim();
    if crate::correccion::palabras(palabra).is_empty() {
        bail!("la palabra clave está vacía");
    }
    let (ruta_pesos, _) = rutas(palabra);
    let cache = |s: &str| ruta_pesos.with_extension(format!("{s}.bin"));
    // IUREVOZ_REUSAR_DATOS=1: reentrena con los ejemplos ya generados (para depurar)
    let (d_ent, d_val, media, desv) = if std::env::var_os("IUREVOZ_REUSAR_DATOS").is_some() && cache("ent").exists() {
        let norm: Vec<f32> = serde_json::from_str(&std::fs::read_to_string(cache("norm").with_extension("json"))?)?;
        (Datos::leer(&cache("ent"))?, Datos::leer(&cache("val"))?, norm[..BANDAS].to_vec(), norm[BANDAS..].to_vec())
    } else {
        let r = generar(palabra, progreso)?;
        std::fs::create_dir_all(ruta_pesos.parent().unwrap())?;
        r.0.guardar(&cache("ent"))?;
        r.1.guardar(&cache("val"))?;
        let norm: Vec<f32> = r.2.iter().chain(&r.3).copied().collect();
        std::fs::write(cache("norm").with_extension("json"), serde_json::to_string(&norm)?)?;
        r
    };
    entrenar_red(palabra, d_ent, d_val, media, desv, progreso)
}

/// Genera y etiqueta los ejemplos: (entrenamiento, validación, media, desviación).
fn generar(palabra: &str, progreso: &dyn Fn(String)) -> Result<(Datos, Datos, Vec<f32>, Vec<f32>)> {

    // 1. audio sintético
    let positivos_txt: Vec<String> = TEXTOS_POSITIVOS.iter().map(|t| t.replace("{}", palabra)).collect();
    let cercanas_txt = cercanas(palabra);
    let (mut pos_ent, mut pos_val, mut cerca_ent, mut cerca_val, mut frases_neg) = (vec![], vec![], vec![], vec![], vec![]);
    let todas: Vec<(&str, bool)> = VOCES_ENTRENAR.iter().map(|v| (*v, true)).chain(VOCES_VALIDAR.iter().map(|v| (*v, false))).collect();
    for (k, (clave, entrenar)) in todas.iter().enumerate() {
        log_paso(progreso, format!("Generando voces sintéticas ({}/{}): {clave}", k + 1, todas.len()));
        // Kokoro es lento en CPU: menos velocidades
        let vel: &[f32] = if clave.starts_with("kokoro") { &[0.9, 1.1] } else { VELOCIDADES };
        let p = sintetizar(clave, &positivos_txt, vel).with_context(|| format!("voz {clave}"))?;
        let c = sintetizar(clave, &cercanas_txt, &[1.0])?;
        if *entrenar {
            pos_ent.extend(p);
            cerca_ent.extend(c);
            if !clave.starts_with("kokoro") {
                let f: Vec<String> = FRASES.iter().map(|s| s.to_string()).collect();
                frases_neg.extend(sintetizar(clave, &f, &[1.0])?);
            }
        } else {
            pos_val.extend(p);
            cerca_val.extend(c);
        }
    }

    // 2. grabaciones reales
    log_paso(progreso, "Leyendo tus grabaciones de depuración…".into());
    let (real_ent, real_val, ruidos) = grabaciones(5);
    let mut aum = Aumento { rng: SmallRng::seed_from_u64(7), ruidos: &ruidos };

    // 3. ventanas etiquetadas
    log_paso(progreso, "Armando ejemplos…".into());
    let mut ent: Vec<(Vec<f32>, f32)> = Vec::new();
    let mut val: Vec<(Vec<f32>, f32)> = Vec::new();
    for clip in &pos_ent {
        for _ in 0..6 {
            let c = aum.variar(clip);
            let fin = aum.fin_positivo();
            ent.push((aum.colocar(&c, fin), 1.0));
        }
        // la palabra cortada a la mitad no cuenta: debe dispararse al terminar de decirla
        let c = aum.variar(clip);
        let mitad = c[..c.len() / 2].to_vec();
        let fin = aum.fin_positivo();
        ent.push((aum.colocar(&mitad, fin), 0.0));
    }
    for clip in &pos_val {
        let c = aum.variar(clip);
        let fin = aum.fin_positivo();
        val.push((aum.colocar(&c, fin), 1.0));
    }
    for clip in &cerca_ent {
        for _ in 0..3 {
            let c = aum.variar(clip);
            let fin = aum.fin_positivo();
            ent.push((aum.colocar(&c, fin), 0.0));
        }
    }
    for clip in &cerca_val {
        let c = aum.variar(clip);
        let fin = aum.fin_positivo();
        val.push((aum.colocar(&c, fin), 0.0));
    }
    for a in &frases_neg {
        for w in ventanas_de(a, SR as usize * 6 / 10) {
            ent.push((w, 0.0));
        }
    }
    ent.extend(real_ent.into_iter().map(|w| (w, 0.0)));
    val.extend(real_val.iter().cloned().map(|w| (w, 0.0)));
    for _ in 0..300 {
        let f = aum.fondo();
        ent.push((f, 0.0));
    }
    let n_pos = ent.iter().filter(|e| e.1 > 0.5).count();
    let n_neg = ent.len() - n_pos;
    log_paso(progreso, format!("Ejemplos: {n_pos} positivos y {n_neg} negativos para entrenar, {} para validar", val.len()));

    // 4. rasgos y normalización
    let mel = LogMel::new();
    let (r_ent, y_ent) = a_rasgos(&mel, &ent);
    let (r_val, y_val) = a_rasgos(&mel, &val);
    drop(ent);
    let mut media = vec![0f32; BANDAS];
    let mut desv = vec![0f32; BANDAS];
    let total: usize = r_ent.iter().map(|r| r.len()).sum();
    for r in &r_ent {
        for m in r {
            for b in 0..BANDAS {
                media[b] += m[b] / total as f32;
            }
        }
    }
    for r in &r_ent {
        for m in r {
            for b in 0..BANDAS {
                desv[b] += (m[b] - media[b]).powi(2) / total as f32;
            }
        }
    }
    desv.iter_mut().for_each(|d| *d = d.sqrt().max(1e-3));
    let d_ent = normalizar(&r_ent, &media, &desv, y_ent);
    let d_val = normalizar(&r_val, &media, &desv, y_val);
    Ok((d_ent, d_val, media, desv))
}

/// Entropía cruzada binaria ponderada, vía sigmoide (con `abs` el gradiente fallaba).
fn bce_ponderada(logits: &Tensor, y: &Tensor, w: &Tensor) -> candle_core::Result<Tensor> {
    let p = candle_nn::ops::sigmoid(logits)?.clamp(1e-6f32, 1.0 - 1e-6)?;
    let uno_menos_y = y.affine(-1.0, 1.0)?;
    let uno_menos_p = p.affine(-1.0, 1.0)?;
    let l = ((y * p.log()?)? + (uno_menos_y * uno_menos_p.log()?)?)?.neg()?;
    (l * w)?.sum_all()? / w.sum_all()?.to_scalar::<f32>()? as f64
}

fn entrenar_red(palabra: &str, d_ent: Datos, d_val: Datos, media: Vec<f32>, desv: Vec<f32>, progreso: &dyn Fn(String)) -> Result<Meta> {
    let mut rng = SmallRng::seed_from_u64(42);
    let n_pos = d_ent.y.iter().filter(|y| **y > 0.5).count();
    let n_neg = d_ent.n - n_pos;

    // 5. entrenamiento
    let dev = Device::Cpu;
    let vm = VarMap::new();
    let red = Red::new(VarBuilder::from_varmap(&vm, DType::F32, &dev))?;
    let mut opt = AdamW::new(vm.all_vars(), ParamsAdamW { lr: 2e-3, weight_decay: 1e-4, ..Default::default() })?;
    let peso_pos = (n_neg as f32 / n_pos.max(1) as f32).clamp(1.0, 10.0);
    let tam = BANDAS * d_ent.marcos;
    let mut orden: Vec<usize> = (0..d_ent.n).collect();
    const EPOCAS: usize = 20;
    const LOTE: usize = 64;
    for epoca in 0..EPOCAS {
        orden.shuffle(&mut rng);
        if epoca == 14 {
            opt.set_learning_rate(5e-4);
        }
        let mut suma = 0f32;
        let mut lotes = 0;
        for trozo in orden.chunks(LOTE) {
            let mut x = Vec::with_capacity(trozo.len() * tam);
            let mut y = Vec::with_capacity(trozo.len());
            for &i in trozo {
                x.extend_from_slice(&d_ent.x[i * tam..(i + 1) * tam]);
                y.push(d_ent.y[i]);
            }
            let xt = Tensor::from_vec(x, (trozo.len(), BANDAS, d_ent.marcos), &dev)?;
            let yt = Tensor::from_vec(y.clone(), trozo.len(), &dev)?;
            let w: Vec<f32> = y.iter().map(|&e| if e > 0.5 { peso_pos } else { 1.0 }).collect();
            let wt = Tensor::from_vec(w, trozo.len(), &dev)?;
            let logits = red.forward(&xt)?;
            let perdida = bce_ponderada(&logits, &yt, &wt)?;
            opt.backward_step(&perdida)?;
            suma += perdida.to_scalar::<f32>()?;
            lotes += 1;
        }
        log_paso(progreso, format!("Entrenando: época {}/{EPOCAS}, pérdida {:.4}", epoca + 1, suma / lotes as f32));
    }

    // 6. umbral: a lo más ~1 falsa activación por hora en tus grabaciones de validación
    let p = puntuar(&red, &d_val, &dev)?;
    let mut neg: Vec<f32> = p.iter().zip(&d_val.y).filter(|(_, y)| **y < 0.5).map(|(s, _)| *s).collect();
    let pos: Vec<f32> = p.iter().zip(&d_val.y).filter(|(_, y)| **y > 0.5).map(|(s, _)| *s).collect();
    neg.sort_by(|a, b| b.total_cmp(a));
    // cada ventana negativa representa 0.5 s de audio; en uso se evalúa cada 0.1 s
    let horas = neg.len() as f32 * 0.5 / 3600.0;
    let permitidas = (horas.max(0.05) * 1.0 / 5.0).floor() as usize; // ÷5: en uso hay 5× más ventanas
    let umbral = neg.get(permitidas).copied().unwrap_or(0.5).max(0.5) + 1e-4;
    let recall = pos.iter().filter(|s| **s >= umbral).count() as f32 / pos.len().max(1) as f32;
    let falsas = neg.iter().filter(|s| **s >= umbral).count() as f32;
    let falsas_por_hora = falsas * 5.0 / horas.max(1e-3);
    log_paso(progreso, format!("Validación: detecta {:.0} % de la palabra en voces nuevas; umbral {umbral:.3}", recall * 100.0));

    let meta = Meta {
        palabra: palabra.to_string(),
        umbral,
        media,
        desv,
        recall,
        falsas_por_hora,
        fecha: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    };
    // un detector que casi no detecta es peor que ninguno: con él la vigilia nunca despierta
    if recall < 0.6 || umbral >= 0.999 {
        bail!(
            "el detector no quedó útil (detecta {:.0} % en voces nuevas); se sigue usando la verificación con Moonshine",
            recall * 100.0
        );
    }
    let (ruta_pesos, ruta_meta) = rutas(palabra);
    std::fs::create_dir_all(ruta_pesos.parent().unwrap())?;
    vm.save(&ruta_pesos)?;
    std::fs::write(&ruta_meta, serde_json::to_string_pretty(&meta)?)?;
    Ok(meta)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// La red debe poder aprender algo trivial: positivos con energía en la banda 5.
    #[test]
    fn aprende_un_caso_facil() -> Result<()> {
        let dev = Device::Cpu;
        let vm = VarMap::new();
        let red = Red::new(VarBuilder::from_varmap(&vm, DType::F32, &dev))?;
        let mut opt = AdamW::new(vm.all_vars(), ParamsAdamW { lr: 2e-3, ..Default::default() })?;
        let t = 118;
        let mut rng = SmallRng::seed_from_u64(1);
        let mut x = vec![0f32; 64 * BANDAS * t];
        let mut y = vec![0f32; 64];
        for i in 0..64 {
            for v in x[i * BANDAS * t..(i + 1) * BANDAS * t].iter_mut() {
                *v = rng.random::<f32>() - 0.5;
            }
            if i % 2 == 0 {
                y[i] = 1.0;
                for j in 50..70 {
                    x[i * BANDAS * t + 5 * t + j] += 3.0;
                }
            }
        }
        let xt = Tensor::from_vec(x, (64, BANDAS, t), &dev)?;
        let yt = Tensor::from_vec(y, 64, &dev)?;
        let mut primera = 0.0;
        let mut ultima = 0.0;
        for paso in 0..150 {
            let logits = red.forward(&xt)?;
            let unos = Tensor::ones(64, DType::F32, &dev)?;
            let perdida = bce_ponderada(&logits, &yt, &unos)?;
            opt.backward_step(&perdida)?;
            let v = perdida.to_scalar::<f32>()?;
            if paso == 0 {
                primera = v;
            }
            ultima = v;
        }
        assert!(ultima < primera * 0.5, "la pérdida no bajó: {primera} → {ultima}");
        Ok(())
    }
}
