# iurevoz

Dictado y control del escritorio por voz para Linux, local y solo CPU. Nuevo miembro del suite Iurefficient (todavía en exploración).

## Uso

```bash
cargo build --release
target/release/iurevoz                 # abre la ventana (la primera vez ofrece bajar los modelos)
target/release/iurevoz alternar        # empieza/detiene el dictado en la ventana abierta
target/release/iurevoz transcribir a.wav [--corregir]   # prueba sin interfaz
target/release/iurevoz comando "abre la calculadora" [--mudo]   # modo comando sin interfaz
target/release/iurevoz decir "hola" --voz piper-claude  # voz local
```

- **Dictar**: botón, F9 con la ventana enfocada, o atajo global. El portal GlobalShortcuts de GNOME 50 en Ubuntu no responde, así que *Ajustes → Instalar atajos de GNOME* (o `iurevoz instalar-atajos`) crea atajos personalizados que ejecutan `iurevoz alternar|comando|leer`: Ctrl+Alt+Espacio, Ctrl+Alt+C y Ctrl+Alt+L.
- **Texto en la ventana activa**: se teclea vía el portal RemoteDesktop de GNOME. La primera vez GNOME pide permiso y se recuerda. En *Ajustes → Probar escritura* se verifica que lleguen acentos, «ñ», «¿?» y comillas.
- **Puntuación dictada**: «coma», «punto», «punto y coma», «dos puntos», «punto y aparte», «dos puntos y aparte», «nueva línea» (o «nuevo/otro renglón», «cambio de renglón», «salto de línea»), «nuevo párrafo», «abre/cierra interrogación», «abre/cierra exclamación», «abre/cierra paréntesis», «abre/cierra comillas», «puntos suspensivos», «viñeta» (salto de línea + «- », que Docs y Word convierten en lista), «viñeta numerada» (salto + «1. », «2. »…; el contador vuelve a 1 en cada párrafo). También se aceptan variantes que el ASR produce, como «punto y seguida» o «punto ya parte». «Punto» y «coma» después de un artículo, preposición u ordinal («el punto de vista», «en coma», «segundo punto») se dejan como palabra.
- **Corrección con LLM** (Ajustes, apagada por omisión): Ollama local, OpenRouter u otro servidor OpenAI-compatible. La API key se toma de `OPENROUTER_API_KEY` o se guarda en `~/.config/iurevoz/config.toml` (permisos 600).
- **Modo comando** (lista cerrada de herramientas, sin shell): botón «Comando», atajo global (se sugiere Ctrl+Alt+C), `iurevoz comando`, o empezar la frase con el prefijo hablado («computadora, abre la calculadora»; decir solo «computadora» deja escuchando la siguiente frase). Un LLM (Gemma 4 E2B local por omisión) elige entre: abrir aplicación, abrir URL, buscar en la web, abrir carpeta de la carpeta personal, calcular, atajos de teclado en la ventana activa, escribir texto, volumen, fecha y hora, leer la selección y leer una página web. Responde en voz alta.
- **Voz (TTS) local**: Piper (México y Argentina), Kokoro y Supertonic 3 vía sherpa-onnx; se elige en Ajustes. Botón «Leer» o atajo (Ctrl+Alt+L) lee el texto seleccionado; pulsarlo de nuevo calla. Mientras la app habla, el micrófono oye silencio para no transcribirse a sí misma.
- Modelos en `~/.local/share/iurevoz/modelos`; también reutiliza los que bajó el benchmark en `~/.cache/stt-bench/models`.
- `IUREVOZ_LOG=1` imprime lo que entrega el ASR, las frases, correcciones y estados en la terminal. `IUREVOZ_GRABAR=1` guarda el audio de cada dictado en `~/.local/share/iurevoz/depuracion/` (solo local).

## Arquitectura (Rust)

```
cpal (16 kHz mono) ─▶ hilo control ─▶ hilo ASR ──────────▶ hilo salida ──▶ portal (teclear)
  botón/F9/atajo/socket ┘      sherpa-onnx + puntuación      corrección LLM      └▶ ventana egui
```

| módulo | qué hace |
|---|---|
| `audio` | captura con cpal; pide 16 kHz mono para que PipeWire remuestree |
| `asr` | Nemotron 560 ms en streaming o Moonshine + VAD Silero. Con Nemotron, un VAD aparte detecta tramos cortos sin tokens de Nemotron («uno», «coma» dichos solos) y los rescata con Moonshine |
| `puntuacion` | comandos de puntuación dictada, mayúsculas y espacios entre frases |
| `correccion` | LLM por frase con contexto, texto delimitado (no se obedece) y salvaguarda de cambios |
| `teclas` | lee la distribución de GNOME con xkbcommon; acentos sin tecla propia → tecla muerta + letra |
| `portal` | RemoteDesktop (teclear y combinaciones de teclas) y GlobalShortcuts (dictar, comando, leer) con ashpd |
| `agente` | modo comando: LLM con *tool calling* (Ollama u OpenAI-compatible), hasta 5 pasos |
| `escritorio` | herramientas permitidas: apps (.desktop + gettext + sinónimos), URLs, carpetas, calculadora, teclas, volumen, lectura |
| `voz` | TTS local con sherpa-onnx y reproducción con cpal, oración por oración e interrumpible |
| `control` | micrófono y órdenes en un hilo propio: en Wayland la ventana no corre si está oculta |
| `motor` | hilos de ASR (latencia crítica) y de salida (corrige e inyecta en orden) |

sherpa-onnx se enlaza estático: el binario (~70 MB) solo depende de libasound, libstdc++ y libxkbcommon. Con Nemotron y el rescate ocupa ~1.5 GB de RAM y transcribe a RTF ≈ 0.29 en un i7-8850H.

## Idea

```
Mic → VAD (Silero) → ASR streaming (sherpa-onnx)
                        │ parciales            │ frase final
                        ▼                      ▼
                   overlay HUD          Router de intención
                                         ├─ Dictado → inyectar texto (X11 / Wayland)
                                         └─ Comando → LLM + herramientas (MCP) → acciones
```

- **ASR**: Nemotron 3.5 ASR Streaming 560 ms (parciales en vivo y texto final). Moonshine base es como opción ligera (menos RAM, pero ~6 s de espera por frase). Todo en CPU.
- **Puntuación**: etapa 1, dictada («coma», «punto», «punto y aparte»…) y convertida con reglas. Etapa 2, corrección con LLM (ver abajo).
- **Activación**: pulsar para hablar (tecla global), conmutar escucha continua, palabra clave opcional (KWS de sherpa-onnx).
- **Inyección de texto**: X11 `xdotool`; Wayland portal RemoteDesktop/libei, `ydotool` o portapapeles + Ctrl+V.
- **Comandos**: LLM con *tool calling* (Gemma/Qwen pequeños vía llama.cpp, o OpenRouter), herramientas tipadas en lista blanca, cliente MCP de las apps del suite (`--mcp`). Confirmación para acciones destructivas; nunca shell libre.
- **Dictado vs. comando**: teclas distintas o prefijo hablado («Comando: …»).

## Resultados del benchmark (2026-10-19)

Un dictado leído de 54 s (texto jurídico y técnico), 4 hilos de CPU, 0.5 s de silencio de cierre:

| modelo | RAM MB | RTF | parcial ms | final ms | WER | CER |
|---|---|---|---|---|---|---|
| nemotron-160ms | 950 | 0.77 | 315 | 779 | 14.5% | 12.0% |
| **nemotron-560ms** | 940 | 0.32 | 748 | 1198 | 2.7% | 1.1% |
| parakeet-v3 | 1287 | 0.12 | — | 1929 | 3.6% | 2.0% |
| moonshine-base-es | 659 | 0.41 | — | 5862 | 1.8% | 1.1% |

Decisión: **Nemotron 560 ms** por omisión y **Moonshine** como alternativa ligera. Nemotron casi no pone puntos en frases largas, de ahí la puntuación dictada.

## Etapa 2: corrección con LLM

Es opcional: sin LLM, la app funciona solo con la etapa 1. La app es autocontenida: trae su propio `llama-server` (llama.cpp) y descarga el modelo GGUF la primera vez, igual que los modelos de ASR. Como el cliente es OpenAI-compatible, también puede apuntar a un servidor externo (Ollama, LM Studio) o a OpenRouter con solo configurarlo. Ollama se usó solo para el benchmark.

- **Cuándo**: por frase, en segundo plano mientras se sigue dictando, con la frase anterior como contexto. El LLM reescribe todo el texto que recibe, así que corregir un bloque largo de una vez tarda demasiado.
- **Qué corrige**: puntuación, mayúsculas y palabras mal oídas, usando un glosario del dominio (jurídico, técnico). No cambia el estilo.
- **Salvaguarda**: comparar contra el original. Aceptar sin más los cambios de puntuación y mayúsculas, y resaltar en el HUD cualquier palabra cambiada. Si cambia demasiado, conservar el original.
- **Privacidad**: con OpenRouter el dictado (posiblemente expedientes) sale de la máquina. Local por omisión; OpenRouter solo si se elige.

Primer benchmark local ([`bench/correccion.py`](bench/correccion.py), 2026-10-19), con Ollama 0.20.3 en un i7-8850H y una Quadro P1000 de 4 GB. Son 11 casos: las salidas de los 4 ASR, el texto sin formato completo y 6 frases sueltas.

| modelo | frase s | párrafo s | tok/s | formato antes→después | arregladas | rotas |
|---|---|---|---|---|---|---|
| qwen3:1.7b | 1.2 | 6.7 | 29 | 12.9% → 13.0% | 0 | 0 |
| qwen3:4b-instruct | 2.3 | 13.2 | 15 | 12.9% → 8.9% | 3 | 4 |
| gemma4 E4B (8B Q4) | 3.1 | 18.1 | 9 | 12.9% → 4.0% | 7 | 5 |

- **Qwen 3 1.7B** no sirve: devuelve el texto casi intacto.
- **Qwen 3 4B** puntúa a medias y mete errores de gramática («agenda» → «agendar»).
- **Gemma 4 E4B** es el único útil. Puntúa bien y arregla «coordenada» → «ordenada» por contexto. Pero inventa: cambió «zinc» por «once» (debía ser «cinco»), borró «el navegador» en un caso y cambió «medio del» por «mediante el». La salvaguarda que resalta las palabras cambiadas es obligatoria.
- **Latencia**: ~3 s por frase con Gemma 4 E4B. Para que no se note, conviene corregir **por frase mientras se sigue dictando**, pasando la frase anterior como contexto. Así solo se espera la última frase, en vez de corregir todo el bloque al soltar la tecla.
- **Pendiente**: con Ollama más nuevo hay modelos que podrían ser mejores y caber completos en la GPU (`qwen3.5:2b`/`4b`, `gemma4:e2b`). Hoy fallan con error 412. Falta probar OpenRouter.

Segunda ronda (Ollama 0.40, mismos 11 casos), ahora con modelos que caben en la GPU y con OpenRouter:

| modelo | frase s | formato antes→después | arregladas | rotas | nota |
|---|---|---|---|---|---|
| ollama gemma4:e2b | 1.0 | 12.9% → 6.5% | 5 | 3 | **mejor local**: rápido y conservador; «zinc» → «cinco» |
| ollama qwen3.5:4b | 3.8 | 12.9% → 4.3% | 4 | 2 | buen formato pero lento; «zinc» → «once» |
| ollama qwen3.5:2b | 1.7 | 12.9% → 9.4% | 0 | 5 | no sirve |
| openrouter deepseek-v4.1-flash | 1.3 | 12.9% → 2.2% | 7 | 2 | **mejor en general** |
| openrouter gemini-3.1-flash-lite | 1.2 | 12.9% → 3.0% | 6 | 3 | |
| openrouter gemma-4-26b-a4b | 1.5 | 12.9% → 3.9% | 4 | 4 | «zinc» → «once» |
| openrouter mistral-small-2603 | 1.1 | 12.9% → 4.8% | 5 | 3 | 2 fallos por tiempo |
| openrouter claude-haiku-5.5 | 1.3 | 12.9% → 30.3% | 4 | 12 | obedeció «abre el navegador…» como orden |

Lección: el texto dictado va delimitado y con la instrucción explícita de no obedecerlo, y la salvaguarda descarta respuestas que no se parecen a la entrada.

## Estado

- [x] Benchmark de modelos ASR → [`bench/`](bench/)
- [x] Benchmark con dictado real y decisión de modelo (provisional; falta más audio y dictado espontáneo)
- [ ] Prueba de inyección de texto en Wayland
- [x] Benchmark de corrección local → [`bench/correccion.py`](bench/correccion.py)
- [x] Corrección: modelos locales nuevos y OpenRouter
- [x] App en Rust con GUI: dictado → puntuación → corrección → teclear en la ventana activa
- [x] Modo comando local con herramientas en lista blanca (sin shell)
- [x] Voz local: respuestas habladas y lectura de selección o páginas
- [ ] Etapa 2: OpenRouter para comandos y Claude Code (cuenta del usuario) para instrucciones que sí toquen el sistema, con confirmación
- [ ] Cliente MCP de las apps del suite
