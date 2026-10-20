# Plan: una voz propia de calidad en español de México

Objetivo: una voz más natural que Piper, Kokoro y Supertonic, con acento mexicano y bien pronunciada en nuestro dominio (cifras, IVA, siglas, términos jurídicos). Restricciones:

- Que corra local en CPU al ritmo de Supertonic 3 o mejor.
- Que la cargue sherpa-onnx 1.13.8 **sin cambios en la app**: solo se agrega una entrada a `VOCES` en `src/voz.rs`.

## Lo que ya sabemos (mediciones del 2026-10-20)

Medición con cuatro frases de dominio en este equipo (12 núcleos, 4 hilos). **RTF**: tiempo de cómputo ÷ duración del audio. **CER**: error de inteligibilidad, medido transcribiendo el audio generado con Moonshine.

| Voz | RTF | CER |
|---|---|---|
| Piper claude / ald | 0.08–0.10 | 2–3 % |
| Supertonic 3 | 0.33 | 0 % |
| Kokoro dora | 1.5 | 0–2 % |
| Pocket TTS español (clonando) | 0.55–0.9 | 7–90 % → **descartado** |

Consecuencias:

- **Kokoro, en este CPU, va más lento que el tiempo real.** Su naturalidad es la mejor del grupo, pero un Kokoro afinado tendría el mismo costo.
- **Piper/VITS tiene margen de sobra en velocidad** (tres veces más rápido que Supertonic). Su prosodia «plana» depende mucho de los datos con que se entrenó, no solo de la arquitectura.

## Qué arquitecturas sirven

| Arquitectura | ¿Se puede entrenar? | ¿sherpa la carga en español? | Comentario |
|---|---|---|---|
| **VITS / Piper** | Sí, receta madura (`OHF-Voice/piper1-gpl`; afinar desde un checkpoint *medium*, p. ej. es_MX-ald) | Sí (`scripts/piper/add_meta_data.py`) | Muy rápida. Es la apuesta segura |
| **Matcha-TTS** + vocoder | Sí (upstream; Projecte Aina lo hizo en catalán) | Sí (espeak, voz en los metadatos) | Algo más natural que VITS, con más trabajo manual |
| **Kokoro** (StyleTTS2) | Receta comunitaria (`kikiri-tts`; precedentes en sueco, alemán y árabe/Nabra) | Sí (`scripts/kokoro/v1.0`) | La más natural, pero RTF 1.5 aquí. Etapa 2 inestable |
| ZipVoice | Sí | **No**: el frontend de 1.13.8 solo maneja zh/en | Descartado |
| Supertonic, Kitten | **No hay código de entrenamiento** | — | Descartados |
| Pocket TTS | Requiere más de 100 h y 16 GB | Sí, con el parche del BOS | Ya evaluado: no conviene |

**Recomendación:** Piper/VITS como línea principal y Matcha como segunda opción; Kokoro solo si aceptamos su lentitud o llega un CPU más rápido.

## De dónde salen los datos

No existe un corpus es-MX de un solo locutor, con calidad de estudio y licencia comercial, ni gratis ni de venta: Common Voice y CIEMPIESS son ruidosos y de muchos hablantes, y Appen y Shaip no tienen uno así. Hay tres caminos:

1. **Destilación (sintético).** Un modelo «maestro» abierto genera de 5 a 10 h con nuestro guion.
   - **Candidatos:**
     - `ResembleAI/Chatterbox-Multilingual-es-mx-latam` (MIT, afinado a México y Latinoamérica).
     - **Qwen3-TTS** (Apache-2.0; la versión 0.6B usa ~2 GB).
   - **Descartados como maestros:**
     - **F5 y XTTS:** no comerciales.
     - **OpenAI, Gemini, Azure y ElevenLabs:** sus términos prohíben, o hacen riesgoso, entrenar con su salida.
   - **Ventaja:** barato y rápido; el alumno aprende exactamente nuestras frases de dominio.
   - **Riesgo:** el alumno hereda los defectos del maestro.
   - **Marca de agua:** Chatterbox marca su audio con PerTh. Habría que verificar con `perth` si el alumno la hereda (es poco probable).
2. **Locutor contratado.** De 3 a 5 h finales, unas 3,000 frases, grabadas a 48 kHz / 24 bit en cuarto tratado y en sesiones con el mismo tono. Es la mejor calidad posible.
   - **Requisito legal:** por la reforma LFDA/LFT (DOF, 14-may-2026), hace falta contrato con consentimiento expreso para entrenar un modelo con su voz.
   - **Qué debe cubrir el contrato:** usos, plazo, remuneración, revocación y aviso de privacidad (LFPDPPP). Debe revisarlo un abogado.
3. **Mixto (recomendado).** Preentrenar o afinar con el corpus sintético y terminar con 1 o 2 h reales del locutor.

**Ojo con la voz del maestro:** si clona a una persona real, aplica lo mismo que con el locutor. Una voz «diseñada» que no es de nadie (la que trae Chatterbox o el diseño por descripción de Qwen3) evita ese tema.

## Fases

| Fase | Qué | Dónde y costo |
|---|---|---|
| **T0 · Banco de pruebas** | `bench/tts_bench.py`: el mismo conjunto de ~30 frases de dominio para cualquier voz; mide RTF, CER (Moonshine/Parakeet), MOS predicho (UTMOS o Distill-MOS, solo para comparar entre voces) y similitud de locutor (ECAPA). El prototipo de hoy ya mide RTF y CER | Local, gratis |
| **T1 · Normalizador de texto** | Cifras, montos, porcentajes, fechas, siglas (IMSS, SAT, CFF) y artículos («art. 14») a palabras antes del TTS. **Mejora también las voces actuales** | Local, gratis |
| **T2 · Elegir maestro** | Generar las 30 frases con Chatterbox es-mx-latam y Qwen3-TTS (en la P1000 o en CPU, aunque sea lento). Escucharlas y pasarlas por T0 | Local, gratis |
| **T3 · Guion** | Unas 3,000 frases: selección greedy por cobertura de difonos sobre texto propio o de dominio público, más vocabulario jurídico, números y preguntas/exclamaciones | Local |
| **T4 · Corpus** | Sintético con el maestro (GPU rentada) y/o grabación del locutor. Limpieza, recorte de silencios y verificación con ASR (se descartan las frases cuyo CER supere un umbral) | ~5–20 USD de GPU; locutor aparte |
| **T5 · Entrenar** | Afinar Piper medium desde es_MX-ald. En paralelo, Matcha | GPU de 24 GB rentada, ~1 día: ~20–100 USD |
| **T6 · Evaluar e integrar** | Comparar con T0 contra Supertonic y Kokoro, y escuchar. Si gana: exportar a ONNX, publicar el `.tar.bz2` en un release propio y agregarlo a `VOCES` | Local |

T0–T3 no cuestan nada y valen la pena aunque al final no se entrene: T1 mejora lo que ya hay y T2 dice cuánta calidad podemos aspirar a destilar.

## Decisiones pendientes

1. **¿De quién es la voz?** Opciones:
   - una voz diseñada (sin persona real);
   - un locutor contratado;
   - la del propio usuario (grabada en silencio).
2. **Presupuesto** para GPU en la nube y, en su caso, para el locutor.
3. **Dónde publicar el modelo** para que la app lo descargue (release en GitHub del proyecto, como `iurefficient-connect`).
