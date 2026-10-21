# /// script
# requires-python = ">=3.10,<3.14"
# dependencies = ["sherpa-onnx==1.13.8", "numpy", "scipy", "soundfile", "num2words"]
# ///
"""Banco de pruebas de TTS en español: las mismas frases de dominio para cualquier voz.

Mide, por voz:
  - RTF: cómputo ÷ duración (solo voces de sherpa-onnx; menor que 1 = más rápido que el tiempo real)
  - CER: se transcribe el audio con Moonshine y se compara con la lectura esperada
    (números a palabras en ambos lados, sin puntuación ni mayúsculas)
  - MOS predicho (opcional, --mos): Distill-MOS. Está entrenado sobre todo con inglés:
    sirve para ordenar voces entre sí, no como cifra absoluta.

Voces:
  - las de la app (Piper, Kokoro, Supertonic) que encuentre en ~/.local/share/iurevoz/voces
    o ~/.cache/stt-bench/tts
  - carpetas de WAV ya generados con --wavs nombre=carpeta (p. ej. salida de un modelo maestro).
    Cada carpeta debe tener 00.wav, 01.wav… en el orden de las frases.

Uso:
  uv run tts_bench.py                                   # todas las voces de la app
  uv run tts_bench.py --solo supertonic,piper-claude
  uv run tts_bench.py --solo ninguna --wavs chatterbox=salidas/chatterbox
  uv run --with distillmos tts_bench.py --mos
  uv run tts_bench.py --normalizar                      # pasa el texto por `iurevoz normalizar` (como la app)
  uv run tts_bench.py --exportar-frases                 # imprime las frases (para generarlas con otro modelo)
"""

import argparse
import json
import os
import re
import subprocess
import time
from pathlib import Path

import numpy as np
import soundfile as sf
from num2words import num2words
from scipy.signal import resample_poly

AQUI = Path(__file__).parent
DIRS_VOCES = [Path.home() / ".local/share/iurevoz/voces", Path.home() / ".cache/stt-bench/tts"]
DIR_ASR = Path.home() / ".cache/stt-bench/models"

# Igual que VOCES en src/voz.rs: (clave, directorio del modelo)
VOCES = [
    ("piper-ald", "vits-piper-es_MX-ald-medium"),
    ("piper-claude", "vits-piper-es_MX-claude-high"),
    ("piper-daniela", "vits-piper-es_AR-daniela-high"),
    ("kokoro-dora", "kokoro-int8-multi-lang-v1_0"),
    ("kokoro-alex", "kokoro-int8-multi-lang-v1_0"),
    ("supertonic", "sherpa-onnx-supertonic-3-tts-int8-2026-05-11"),
]


def leer_frases(ruta: Path) -> list[tuple[str, str]]:
    frases = []
    for linea in ruta.read_text(encoding="utf-8").splitlines():
        if not linea.strip() or linea.startswith("#"):
            continue
        texto, _, lectura = linea.partition("\t")
        frases.append((texto.strip(), (lectura or texto).strip()))
    return frases


def _numero(m: re.Match) -> str:
    s = m.group(0).replace(",", "")
    try:
        return " " + (num2words(float(s), lang="es") if "." in s else num2words(int(s), lang="es")) + " "
    except (ValueError, OverflowError):
        return s


def normalizar(t: str) -> str:
    t = t.lower().replace("%", " por ciento ").replace("$", " ")
    t = re.sub(r"\b(\d{1,3})((?:\.\d{3})+)\b", lambda m: m.group(1) + m.group(2).replace(".", ","), t)  # 1.160 → 1,160
    t = re.sub(r"\d[\d,]*(\.\d+)?", _numero, t)
    t = re.sub(r"[^\wáéíóúüñ ]", " ", t)
    return " ".join(t.split())


def cer(ref: str, hip: str) -> float:
    a, b = normalizar(ref), normalizar(hip)
    d = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        prev, d[0] = d[0], i
        for j, cb in enumerate(b, 1):
            prev, d[j] = d[j], min(d[j] + 1, d[j - 1] + 1, prev + (ca != cb))
    # tope de 1: una alucinación del ASR (repetir «88» veinte veces) no debe dominar el promedio
    return min(1.0, d[-1] / max(1, len(a)))


def buscar(sub: str) -> Path | None:
    return next((d / sub for d in DIRS_VOCES if (d / sub).exists()), None)


def cargar_voz(so, clave: str, d: Path, hilos: int):
    """Devuelve (OfflineTts, GenerationConfig) con la misma configuración que src/voz.rs."""
    gc = so.GenerationConfig()
    uno = lambda pat: str(next(d.glob(pat)))
    if clave.startswith("piper"):
        m = so.OfflineTtsModelConfig(vits=so.OfflineTtsVitsModelConfig(
            model=uno("*.onnx"), tokens=str(d / "tokens.txt"), data_dir=str(d / "espeak-ng-data")), num_threads=hilos)
    elif clave.startswith("kokoro"):
        m = so.OfflineTtsModelConfig(kokoro=so.OfflineTtsKokoroModelConfig(
            model=str(d / "model.int8.onnx"), voices=str(d / "voices.bin"), tokens=str(d / "tokens.txt"),
            data_dir=str(d / "espeak-ng-data"), lang="es"), num_threads=hilos)
        gc.sid = 29 if clave == "kokoro-alex" else 28
    else:
        m = so.OfflineTtsModelConfig(supertonic=so.OfflineTtsSupertonicModelConfig(
            duration_predictor=uno("duration_predictor*"), text_encoder=uno("text_encoder*"),
            vector_estimator=uno("vector_estimator*"), vocoder=uno("vocoder*"), tts_json=str(d / "tts.json"),
            unicode_indexer=str(d / "unicode_indexer.bin"), voice_style=str(d / "voice.bin")), num_threads=hilos)
        gc.extra = {"lang": "es"}
    return so.OfflineTts(so.OfflineTtsConfig(model=m)), gc


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--frases", type=Path, default=AQUI / "frases_tts.tsv")
    p.add_argument("--solo", default="", help="voces de la app a probar, separadas por coma ('ninguna' = solo --wavs)")
    p.add_argument("--wavs", action="append", default=[], help="nombre=carpeta con 00.wav, 01.wav…")
    p.add_argument("--hilos", type=int, default=min(4, os.cpu_count() or 4))
    p.add_argument("--mos", action="store_true", help="MOS predicho con Distill-MOS (uv run --with distillmos)")
    p.add_argument("--salida", type=Path, default=AQUI / "tts-salidas", help="aquí se guardan los WAV generados")
    p.add_argument("--json", type=Path, help="guarda los resultados por frase")
    p.add_argument("--mostrar-texto", action="store_true", help="imprime lo que entendió el ASR cuando hay errores")
    p.add_argument("--normalizar", nargs="?", const=str(AQUI.parent / "target/release/iurevoz"), metavar="BIN",
                   help="normaliza el texto con `BIN normalizar` antes de sintetizar (por omisión, el binario de release)")
    p.add_argument("--exportar-frases", action="store_true", help="imprime el texto de las frases y termina")
    a = p.parse_args()

    frases = leer_frases(a.frases)
    if a.normalizar:
        r = subprocess.run([a.normalizar, "normalizar"], input="\n".join(t for t, _ in frases), capture_output=True, text=True, check=True)
        frases = [(t, lectura) for t, (_, lectura) in zip(r.stdout.splitlines(), frases)]
    if a.exportar_frases:
        print("\n".join(t for t, _ in frases))
        return

    import sherpa_onnx as so

    moon = next(DIR_ASR.glob("*moonshine-base-es*"), None)
    if moon is None:
        raise SystemExit(f"falta Moonshine en {DIR_ASR}: corre antes `uv run stt_bench.py --descargar`")
    asr = so.OfflineRecognizer.from_moonshine_v2(
        encoder=str(next(moon.glob("encoder*.ort"))), decoder=str(next(moon.glob("decoder*.ort"))),
        tokens=str(moon / "tokens.txt"), num_threads=a.hilos)

    mos = None
    if a.mos:
        import distillmos
        import torch
        modelo_mos = distillmos.ConvTransformerSQAModel().eval()
        mos = lambda x16: float(modelo_mos(torch.from_numpy(x16)[None]).item())

    def transcribir(x: np.ndarray, sr: int) -> str:
        st = asr.create_stream()
        st.accept_waveform(16000, resample_poly(x, 16000, sr).astype(np.float32))
        asr.decode_stream(st)
        return st.result.text

    # (nombre, función que devuelve (audio, sr, segundos de cómputo) para la frase i)
    voces = []
    elegidas = {s.strip() for s in a.solo.split(",") if s.strip()}
    for clave, sub in VOCES:
        if elegidas and clave not in elegidas:
            continue
        d = buscar(sub)
        if d is None:
            print(f"(omito {clave}: no encontré {sub})")
            continue

        def generar(i, clave=clave, d=d, cache={}):
            if "tts" not in cache:
                cache["tts"], cache["gc"] = cargar_voz(so, clave, d, a.hilos)
                cache["tts"].generate("Hola.", cache["gc"])  # calentamiento
            t0 = time.perf_counter()
            au = cache["tts"].generate(frases[i][0], cache["gc"])
            return np.asarray(au.samples, dtype=np.float32), au.sample_rate, time.perf_counter() - t0

        voces.append((clave, generar))
    for w in a.wavs:
        nombre, _, carpeta = w.partition("=")
        def leer(i, carpeta=Path(carpeta)):
            x, sr = sf.read(carpeta / f"{i:02d}.wav", dtype="float32", always_2d=True)
            return x[:, 0], sr, None
        voces.append((nombre, leer))

    resultados = {}
    print(f"{'voz':22} {'RTF':>5} {'CER':>6} {'CER p90':>8} {'MOS':>5}  frases={len(frases)} hilos={a.hilos}")
    for nombre, fuente in voces:
        filas, comp, dur = [], 0.0, 0.0
        (a.salida / nombre).mkdir(parents=True, exist_ok=True)
        for i, (texto, lectura) in enumerate(frases):
            x, sr, seg = fuente(i)
            if seg is not None:
                comp += seg
                dur += len(x) / sr
                sf.write(a.salida / nombre / f"{i:02d}.wav", x, sr)
            hip = transcribir(x, sr)
            fila = {"i": i, "texto": texto, "asr": hip, "cer": cer(lectura, hip)}
            if mos:
                fila["mos"] = mos(resample_poly(x, 16000, sr).astype(np.float32))
            filas.append(fila)
            if a.mostrar_texto and fila["cer"] > 0.05:
                print(f"    {i:02d} cer={fila['cer']:.2f} | {lectura}\n                 → {hip}")
        cs = [f["cer"] for f in filas]
        rtf = f"{comp / dur:5.2f}" if dur else "    –"
        m = f"{np.mean([f['mos'] for f in filas]):5.2f}" if mos else "    –"
        print(f"{nombre:22} {rtf} {np.mean(cs):6.3f} {np.percentile(cs, 90):8.3f} {m}", flush=True)
        resultados[nombre] = {"rtf": comp / dur if dur else None, "cer": float(np.mean(cs)), "frases": filas}

    if a.json:
        a.json.write_text(json.dumps(resultados, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
