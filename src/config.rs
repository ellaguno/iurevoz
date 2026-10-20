//! Configuración persistente en ~/.config/iurevoz/config.toml.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "mx.iurefficient.IureVoz";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModeloAsr {
    /// Nemotron 3.5 ASR Streaming 560 ms: texto en vivo y final en ~1.2 s.
    Nemotron,
    /// Moonshine base es: menos RAM, sin parciales, final más lento.
    Moonshine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Inyeccion {
    /// Teclea en la ventana activa vía el portal RemoteDesktop.
    Teclear,
    /// Solo deja el texto en la ventana de iurevoz.
    Ninguna,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Servicio {
    /// llama-server administrado por la app (local, sin instalar nada).
    Local,
    /// API nativa de Ollama (local).
    Ollama,
    /// OpenRouter (nube): el texto sale de la máquina.
    OpenRouter,
    /// Cualquier servidor OpenAI-compatible (llama-server, LM Studio…).
    OpenAi,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Correccion {
    pub activa: bool,
    pub servicio: Servicio,
    pub url: String,
    pub modelo: String,
    /// Variable de entorno con la API key (solo OpenRouter / OpenAI). Tiene prioridad.
    pub variable_clave: String,
    /// API key guardada (para cuando la app se abre desde el menú, sin el entorno del shell).
    pub clave: Option<String>,
    /// Términos del dominio que el modelo debe respetar.
    pub glosario: Vec<String>,
    /// Si el modelo cambia más de esta fracción de palabras, se conserva el original.
    pub max_cambio: f32,
}

impl Default for Correccion {
    fn default() -> Self {
        Self {
            activa: false,
            servicio: Servicio::Local,
            url: String::new(),
            modelo: "gemma4-e2b".into(),
            variable_clave: "OPENROUTER_API_KEY".into(),
            clave: None,
            glosario: vec![],
            max_cambio: 0.25,
        }
    }
}

impl Correccion {
    pub fn clave_efectiva(&self) -> Option<String> {
        std::env::var(&self.variable_clave).ok().filter(|c| !c.is_empty()).or_else(|| self.clave.clone().filter(|c| !c.is_empty()))
    }

    pub fn url_por_omision(servicio: Servicio) -> &'static str {
        match servicio {
            Servicio::Local => "",
            Servicio::Ollama => "http://localhost:11434",
            Servicio::OpenRouter => "https://openrouter.ai/api/v1",
            Servicio::OpenAi => "http://localhost:8080/v1",
        }
    }
}

/// Modo comando: instrucciones habladas que un LLM traduce a acciones de escritorio.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Comandos {
    pub activos: bool,
    /// LLM que interpreta (mismos campos que la corrección).
    pub llm: Correccion,
    /// Etapa 2: permitir delegar a Claude Code (cuenta del usuario), con confirmación.
    pub claude: bool,
}

impl Default for Comandos {
    fn default() -> Self {
        // para instrucciones conviene un modelo que razone mejor aunque sea más lento
        let llm = Correccion { activa: true, modelo: "qwen3.5-4b".into(), ..Correccion::default() };
        Self { activos: true, llm, claude: true }
    }
}

/// Modo «siempre escuchando» con palabra clave.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Escucha {
    pub siempre: bool,
    pub palabra: String,
    /// 0 = exacto; más alto acepta más variantes (y más falsas activaciones).
    pub tolerancia: f32,
}

impl Default for Escucha {
    fn default() -> Self {
        Self { siempre: false, palabra: "oye compu".into(), tolerancia: 0.34 }
    }
}

/// Voz (TTS) local.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VozCfg {
    /// Responder en voz alta a los comandos.
    pub responder: bool,
    pub voz: String,
    pub velocidad: f32,
}

impl Default for VozCfg {
    fn default() -> Self {
        Self { responder: true, voz: "piper-claude".into(), velocidad: 1.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub modelo_asr: ModeloAsr,
    pub hilos: i32,
    /// Silencio (s) que cierra una frase.
    pub silencio: f32,
    pub puntuacion_dictada: bool,
    pub inyeccion: Inyeccion,
    pub correccion: Correccion,
    pub comandos: Comandos,
    pub voz: VozCfg,
    pub escucha: Escucha,
    /// Token del portal RemoteDesktop para no volver a pedir permiso.
    pub token_portal: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modelo_asr: ModeloAsr::Nemotron,
            hilos: 4,
            silencio: 0.5,
            puntuacion_dictada: true,
            inyeccion: Inyeccion::Teclear,
            correccion: Correccion::default(),
            comandos: Comandos::default(),
            voz: VozCfg::default(),
            escucha: Escucha::default(),
            token_portal: None,
        }
    }
}

pub fn dir_config() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("iurevoz")
}

pub fn dir_datos() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("iurevoz")
}

impl Config {
    fn ruta() -> PathBuf {
        dir_config().join("config.toml")
    }

    pub fn cargar() -> Self {
        match std::fs::read_to_string(Self::ruta()) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("config.toml inválido, uso valores por omisión: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn guardar(&self) -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir_config())?;
        let ruta = Self::ruta();
        std::fs::write(&ruta, toml::to_string_pretty(self)?)?;
        // puede contener la API key
        std::fs::set_permissions(&ruta, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}
