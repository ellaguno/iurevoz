//! iurevoz: dictado por voz local para Linux.

mod agente;
mod asr;
mod atajos_gnome;
mod audio;
mod claude;
mod config;
mod control;
mod correccion;
mod detector;
mod entrenamiento;
mod escritorio;
mod iure;
mod llm_local;
mod modelos;
mod motor;
mod portal;
mod procesos;
mod puntuacion;
mod resumen;
mod rasgos;
mod suite;
mod teclas;
mod ui;
mod vigilia;
mod voz;

use std::time::Instant;

use anyhow::{Context, Result, bail};

use config::Config;

const AYUDA: &str = "\
iurevoz — dictado por voz local

Uso:
  iurevoz                       abre la ventana
  iurevoz alternar              empieza/detiene el dictado en la instancia abierta
  iurevoz transcribir A.wav [--corregir]
                                prueba sin interfaz: ASR + puntuación (+ corrección)
  iurevoz comando \"instrucción\"   ejecuta una instrucción sin interfaz (prueba del modo comando)
  iurevoz instalar-atajos       crea atajos de GNOME (Ctrl+Alt+Espacio, Ctrl+Alt+C, Ctrl+Alt+L)
  iurevoz leer | callar         lee la selección en voz alta en la instancia abierta
  iurevoz entrenar-palabra [frase]  entrena el detector dedicado de la palabra clave
  iurevoz vigilia A.wav         simula «siempre escuchando» con un archivo
  iurevoz preparar-llm [modelo] descarga llama.cpp y el modelo local (gemma4-e2b, qwen3.5-4b, qwen3.5-2b)
  iurevoz conectar-iurefficient crea el token de IureVoz con la sesión de la suite
  iurevoz decir \"texto\" [--voz CLAVE] [--guardar A.wav]
                                habla con la voz local (claves: piper-ald, piper-claude,
                                piper-daniela, kokoro-dora, kokoro-alex, supertonic)
  iurevoz --sin-portal          abre la ventana sin teclear ni atajo global
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "ayuda") => print!("{AYUDA}"),
        Some(o @ ("alternar" | "leer" | "callar" | "rechazar")) if args.len() == 1 => {
            if !control::enviar_a_instancia(o) {
                bail!("iurevoz no está abierto");
            }
        }
        Some("mcp-permisos") => claude::servidor_permisos()?,
        Some("claude") => {
            let tarea = args[1..].join(" ");
            let r = claude::ejecutar(&tarea, |p| match p {
                claude::Progreso::Texto(t) => println!("  … {t}"),
                claude::Progreso::Herramienta(h) => println!("  · {h}"),
            })?;
            println!("{r}");
        }
        Some("preparar-llm") => {
            let clave = args.get(1).cloned().unwrap_or_else(|| "gemma4-e2b".into());
            llm_local::preparar(&clave, &|q, f| eprint!("\r{q} {:.0}%   ", f * 100.0))?;
            eprintln!();
            let t = Instant::now();
            let url = llm_local::url(&clave)?;
            println!("llama-server listo en {:.1} s: {url}", t.elapsed().as_secs_f32());
            llm_local::apagar();
        }
        Some("conectar-iurefficient") => println!("{}", iure::conectar()?),
        Some("instalar-atajos") => println!("{}", atajos_gnome::instalar()?),
        Some("comando") if args.len() == 1 => {
            if !control::enviar_a_instancia("comando") {
                bail!("iurevoz no está abierto");
            }
        }
        Some("comando") => {
            let cfg = Config::cargar();
            let mudo = args.iter().any(|a| a == "--mudo");
            let instruccion = args[1..].iter().filter(|a| *a != "--mudo").cloned().collect::<Vec<_>>().join(" ");
            let voz = (cfg.voz.responder && !mudo).then(|| voz::Voz::iniciar(cfg.voz.voz.clone(), cfg.voz.velocidad, |m| eprintln!("{m}")));
            let portal = portal::Portal::iniciar(cfg.token_portal.clone(), false, |a| {
                if let portal::Aviso::TokenTeclado(t) = a {
                    let mut c = Config::cargar();
                    c.token_portal = Some(t);
                    let _ = c.guardar();
                }
            });
            let ag = agente::Agente::new(cfg.comandos.llm.clone(), Some(portal), voz.clone());
            let t = Instant::now();
            ag.precalentar()?;
            if cfg.comandos.llm.servicio == config::Servicio::Local {
                println!("(LLM local precalentado en {:.1} s)", t.elapsed().as_secs_f32());
            }
            let t = Instant::now();
            // «a ;; b»: varias instrucciones seguidas, con memoria, para probar la continuidad
            let mut historial = Vec::new();
            let mut inf = agente::Informe::default();
            for i in instruccion.split(";;").map(str::trim).filter(|i| !i.is_empty()) {
                println!("> {i}");
                inf = ag.ejecutar(i, &historial, |a| println!("  · {a}"))?;
                println!("{}  ({:.1} s)", inf.respuesta, t.elapsed().as_secs_f32());
                historial.push(serde_json::json!({"role": "user", "content": i}));
                historial.push(serde_json::json!({"role": "assistant", "content": inf.respuesta.clone()}));
            }
            if let Some(v) = voz {
                v.decir(inf.respuesta);
                // espera a que termine de hablar
                std::thread::sleep(std::time::Duration::from_millis(300));
                while v.hablando.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            // da tiempo a que el portal termine de teclear
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Some("mi-dia") => {
            // «¿qué tengo hoy?» desde la terminal (o un atajo que lo pase a `notify-send`)
            println!("{}", resumen::mi_dia()?);
        }
        Some("decir") => {
            let texto = args.get(1).context("falta el texto")?;
            let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
            let clave = opt("--voz").unwrap_or_else(|| "piper-ald".into());
            if voz::falta(&clave).is_some() {
                eprintln!("Descargando la voz {clave}…");
                voz::descargar(&clave, |f| eprint!("\r{:.0}%   ", f * 100.0))?;
                eprintln!();
            }
            let t = Instant::now();
            let s = voz::Sintetizador::cargar(&clave, 1.0)?;
            let carga = t.elapsed().as_secs_f32();
            let t = Instant::now();
            let (m, sr) = s.generar(texto)?;
            let tgen = t.elapsed().as_secs_f32();
            let dur = m.len() as f32 / sr as f32;
            eprintln!("{clave}: carga {carga:.1} s · {dur:.1} s de audio en {tgen:.2} s (RTF {:.2})", tgen / dur);
            match opt("--guardar") {
                Some(r) => drop(sherpa_onnx::write(&r, &m, sr)),
                None => voz::reproducir(&m, sr, &std::sync::atomic::AtomicBool::new(false))?,
            }
        }
        Some("entrenar-palabra") => {
            let palabra = if args.len() > 1 { args[1..].join(" ") } else { Config::cargar().escucha.palabra };
            let t = Instant::now();
            let m = entrenamiento::entrenar(&palabra, &|m| eprintln!("{m}"))?;
            println!(
                "Detector de «{}» listo en {:.0} s: detecta {:.0} % en voces no vistas, umbral {:.3}, ~{:.1} falsas/hora en tus grabaciones",
                m.palabra,
                t.elapsed().as_secs_f32(),
                m.recall * 100.0,
                m.umbral,
                m.falsas_por_hora
            );
        }
        Some("vigilia") => {
            let ruta = args.get(1).context("falta el archivo .wav")?;
            probar_vigilia(ruta)?;
        }
        Some("transcribir") => {
            let ruta = args.get(1).context("falta el archivo .wav")?;
            transcribir(ruta, args.iter().any(|a| a == "--corregir"))?;
        }
        Some(otro) if !otro.starts_with("--") => bail!("subcomando desconocido: {otro} (ver iurevoz --help)"),
        _ => {
            if control::enviar_a_instancia("alternar") {
                // ya hay una ventana abierta: el segundo arranque funciona como atajo
                return Ok(());
            }
            instalar_lanzador();
            let sin_portal = args.iter().any(|a| a == "--sin-portal");
            let r = ui::ejecutar(Config::cargar(), sin_portal);
            llm_local::apagar();
            suite::apagar();
            r.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    Ok(())
}

/// Los portales identifican a las apps fuera de Flatpak por su archivo .desktop.
fn instalar_lanzador() {
    let Some(dir) = dirs::data_dir().map(|d| d.join("applications")) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    let ruta = dir.join(format!("{}.desktop", config::APP_ID));
    let contenido = format!(
        "[Desktop Entry]\nType=Application\nName=IureVoz\nComment=Dictado por voz local\n\
         Exec={}\nIcon=audio-input-microphone\nCategories=Utility;Accessibility;\nTerminal=false\n",
        exe.display()
    );
    if std::fs::read_to_string(&ruta).ok().as_deref() != Some(contenido.as_str()) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&ruta, contenido);
    }
}

/// Simula un dictado con un archivo: bloques de 100 ms como si vinieran del micrófono.
fn transcribir(ruta: &str, corregir: bool) -> Result<()> {
    let cfg = Config::cargar();
    let faltan = modelos::faltantes(cfg.modelo_asr);
    if !faltan.is_empty() {
        eprintln!("Descargando {faltan:?}…");
        modelos::descargar(cfg.modelo_asr, |p, f| eprint!("\r{p} {:.0}%   ", f * 100.0))?;
        eprintln!();
    }
    let wav = sherpa_onnx::Wave::read(ruta).context("no pude leer el WAV (16 kHz mono)")?;
    let t = Instant::now();
    let mut rec = asr::Reconocedor::cargar(&cfg)?;
    eprintln!("{:?} cargado en {:.1} s", cfg.modelo_asr, t.elapsed().as_secs_f32());

    let mut muestras = wav.samples().to_vec();
    if wav.sample_rate() != asr::SR {
        let rs = sherpa_onnx::LinearResampler::create(wav.sample_rate(), asr::SR).context("remuestreo")?;
        muestras = rs.resample(&muestras, true);
    }
    let corrector = corregir.then(|| correccion::Corrector::new(cfg.correccion.clone()));
    let mut punt = puntuacion::Puntuador::new();
    let mut texto = String::new();
    let t = Instant::now();
    let mut salidas = Vec::new();
    for bloque in muestras.chunks(asr::SR as usize / 10) {
        salidas.extend(rec.alimentar(bloque));
    }
    salidas.extend(rec.terminar());
    let computo = t.elapsed().as_secs_f32();
    for s in salidas {
        if let asr::Salida::Final(f) = s {
            let p = punt.procesar(&f, cfg.puntuacion_dictada);
            println!("ASR   {f}");
            let mut final_ = p.clone();
            if let Some(c) = &corrector {
                let inicio = p.len() - p.trim_start().len();
                let r = c.corregir(&p[inicio..], &texto)?;
                println!("LLM   {} ({} ms, {}){}", r.texto, r.ms, if r.aceptada { "aceptada" } else { "DESCARTADA" },
                    if r.cambios.is_empty() { String::new() } else { format!(" cambios: {:?}", r.cambios) });
                final_ = format!("{}{}", &p[..inicio], r.texto);
            }
            texto.push_str(&final_);
        }
    }
    let dur = muestras.len() as f32 / asr::SR as f32;
    println!("\n{}\n", texto.trim());
    eprintln!("audio {dur:.1} s · cómputo ASR {computo:.1} s · RTF {:.2}", computo / dur);
    Ok(())
}

/// Simula el modo «siempre escuchando» con un archivo: muestra cuándo despierta, qué
/// instrucción entiende y cuánto CPU gasta la espera.
fn probar_vigilia(ruta: &str) -> Result<()> {
    let cfg = Config::cargar();
    let wav = sherpa_onnx::Wave::read(ruta).context("no pude leer el WAV")?;
    let mut muestras = wav.samples().to_vec();
    if wav.sample_rate() != asr::SR {
        muestras = sherpa_onnx::LinearResampler::create(wav.sample_rate(), asr::SR).context("remuestreo")?.resample(&muestras, true);
    }
    let mut rec = asr::Reconocedor::cargar(&cfg)?;
    let mut vig = vigilia::Vigilia::new(&modelos::rutas_moonshine()?.vad)?;
    let (palabra, tol) = (cfg.escucha.palabra.clone(), cfg.escucha.tolerancia);
    let sin_detector = std::env::args().any(|a| a == "--moonshine");
    if !sin_detector && vig.cargar_detector(&palabra) {
        println!("Detector dedicado de «{palabra}»");
    } else {
        println!("Verificación con Moonshine");
    }
    let paso = asr::SR as usize / 10;
    let cpu = || {
        let mut u = unsafe { std::mem::zeroed::<Rusage>() };
        unsafe { getrusage(0, &mut u) };
        u.ru_utime[0] as f64 + u.ru_utime[1] as f64 / 1e6 + u.ru_stime[0] as f64 + u.ru_stime[1] as f64 / 1e6
    };
    let (mut cpu_vigilia, mut seg_vigilia) = (0.0, 0.0);
    let mut i = 0;
    while i < muestras.len() {
        let fin = (i + paso).min(muestras.len());
        let c0 = cpu();
        let despierta = vig.alimentar(&muestras[i..fin], |tramo| {
            let t = rec.transcribir_corto(tramo);
            let si = vigilia::coincide(&t, &palabra, tol).is_some();
            println!("  {:6.1} s  revisa {t:?}{}", i as f32 / asr::SR as f32, if si { "  → DESPIERTA" } else { "" });
            si
        });
        cpu_vigilia += cpu() - c0;
        seg_vigilia += (fin - i) as f64 / asr::SR as f64;
        i = fin;
        if let Some(audio) = despierta {
            // modo comando: se junta la instrucción hasta ~1 s de silencio tras la última frase
            let mut texto = String::new();
            let mut salidas = rec.alimentar(&audio);
            // mismo criterio que la app: termina ~1 s después de la última frase, si no hay voz
            let mut desde_final: Option<usize> = salidas.iter().any(|x| matches!(x, asr::Salida::Final(_))).then_some(0);
            let mut hablando = false;
            let mut esperado = 0usize;
            while i < muestras.len() {
                let fin = (i + paso).min(muestras.len());
                let s = rec.alimentar(&muestras[i..fin]);
                for x in &s {
                    match x {
                        asr::Salida::Final(_) => {
                            desde_final = Some(0);
                            hablando = false;
                        }
                        asr::Salida::Parcial(p) => hablando = !p.is_empty(),
                    }
                }
                salidas.extend(s);
                let n = fin - i;
                i = fin;
                esperado += n;
                if let Some(d) = desde_final.as_mut() {
                    if !hablando {
                        *d += n;
                    }
                    if *d >= asr::SR as usize {
                        break;
                    }
                } else if esperado >= asr::SR as usize * 8 {
                    break; // nadie dijo nada en 8 s
                }
            }
            salidas.extend(rec.terminar());
            for s in salidas {
                if let asr::Salida::Final(f) = s {
                    texto.push(' ');
                    texto.push_str(&f);
                }
            }
            let instr = vigilia::quitar(texto.trim(), &palabra, tol).unwrap_or_else(|| texto.trim().to_string());
            println!("  {:6.1} s  DESPIERTA · instrucción: {instr:?}", i as f32 / asr::SR as f32);
            vig.reiniciar();
        }
    }
    println!("\nEsperando la palabra clave: {:.2}% de un núcleo ({cpu_vigilia:.2} s de CPU en {seg_vigilia:.0} s de audio)", 100.0 * cpu_vigilia / seg_vigilia.max(1e-9));
    Ok(())
}

/// `struct rusage` de Linux x86-64 (solo se leen los tiempos de CPU del proceso).
#[repr(C)]
struct Rusage {
    ru_utime: [i64; 2],
    ru_stime: [i64; 2],
    _resto: [i64; 14],
}

unsafe extern "C" {
    fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}
