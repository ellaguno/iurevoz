# stt-bench

Benchmark de STT casi en tiempo real **solo CPU** para dictado en español, con sherpa-onnx 1.13.8 (la misma versión que el diarizador de IureTranscribe).

| clave | modelo | modo |
|---|---|---|
| `nemotron-160ms`, `nemotron-560ms` | Nemotron 3.5 ASR Streaming 0.6B int8 | streaming real con endpoint propio |
| `parakeet-v3` | Parakeet TDT 0.6B v3 int8 | offline, frases cortadas con VAD Silero |
| `moonshine-base-es` | Moonshine base es (cuantizado) | offline, frases cortadas con VAD Silero |

## Uso

```bash
uv run stt_bench.py --descargar     # ~1.7 GB en ~/.cache/stt-bench/models
uv run stt_bench.py --grabar        # te muestra un texto, lo lees y graba con arecord
uv run stt_bench.py grabaciones/*.wav --mostrar-texto --json resultados.json
```

- Acepta cualquier audio que entienda ffmpeg. Si existe `<audio>.txt` junto al archivo, se usa como referencia para WER/CER.
- `--solo nemotron-160,parakeet` filtra modelos, `--hilos N` (4 por omisión), `--silencio 0.5` es el silencio que cierra una frase e `--idioma es|auto`.
- Cada modelo corre en su propio proceso, así que la RAM medida es solo la suya.

## Cómo leer la tabla

- **RTF**: cómputo ÷ duración. Menor que 1 significa que va más rápido que el tiempo real. En streaming tiene que quedar holgadamente por debajo de 1, porque si no se va atrasando.
- **parcial ms**: tiempo hasta que aparece texto mientras hablas (bloque del modelo + cómputo p95). Solo aplica a Nemotron. Los offline no muestran nada hasta cerrar la frase.
- **final ms**: tiempo desde que dejas de hablar hasta el texto definitivo.
- **WER/CER**: sin mayúsculas ni puntuación y conservando acentos. Los números ("5" vs "cinco") cuentan como error, por eso el texto de lectura no los incluye.

## Notas

- En Nemotron el silencio de cierre tiene que cubrir *silencio + un bloque*. Con 560 ms y solo 0.5 s de silencio, el endpoint corta las últimas palabras, porque el decodificador va un bloque atrás. El script ya lo compensa.
- Más hilos no siempre es mejor: con bloques pequeños, 8 hilos rindió peor que 4.
- Los audios de prueba que traen los modelos duran ~5 s y no sirven para decidir. Graba al menos 1–2 minutos de dictado real (`--grabar` varias veces).

## Corrección con LLM (`correccion.py`)

Toma `resultados.json` de `stt_bench.py` y le pide a cada modelo que corrija la salida de cada ASR, el texto completo sin formato y cada frase suelta.

```bash
uv run correccion.py ollama:gemma4:latest ollama:qwen3:4b-instruct --mostrar --json correccion.json
OPENROUTER_API_KEY=… uv run correccion.py openrouter:google/gemma-3-4b-it
```

- **arregladas / rotas**: palabras mal oídas que el modelo dejó bien, y palabras correctas que echó a perder. «rotas» es la cifra que más importa.
- **formato**: tasa de error contando signos y mayúsculas. **WER** ignora ambos, como en `stt_bench.py`.
- `--glosario archivo.txt` añade términos del dominio a las instrucciones del modelo; `--solo frase` filtra casos.
- Con OpenRouter el texto sale de la máquina. Úsalo solo con el texto de lectura, no con dictados reales.
