# /// script
# requires-python = ">=3.10,<3.14"
# dependencies = ["sherpa-onnx==1.13.8", "numpy"]
# ///
"""Benchmark de STT casi en tiempo real (solo CPU) para dictado en español.

Compara, con sherpa-onnx, los modelos que encuentre en el directorio de modelos:
  - Nemotron 3.5 ASR Streaming 0.6B (streaming real, una entrada por tamaño de bloque)
  - Parakeet TDT 0.6B v3 (offline, segmentado con VAD Silero)
  - Moonshine base es (offline, segmentado con VAD Silero)

Uso:
  uv run stt_bench.py --descargar            # baja modelos a ~/.cache/stt-bench/models
  uv run stt_bench.py --grabar               # te grabas leyendo un texto de referencia
  uv run stt_bench.py audio1.wav audio2.m4a  # referencia opcional: audio1.txt junto al audio
"""

import argparse
import concurrent.futures as cf
import datetime as dt
import json
import multiprocessing as mp
import os
import re
import resource
import subprocess
import sys
import tarfile
import time
import unicodedata
import urllib.request
from pathlib import Path

import numpy as np

SR = 16000
DIR_MODELOS = Path.home() / ".cache" / "stt-bench" / "models"
URL_BASE = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
DESCARGAS = [
    "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-160ms-int8-2026-06-11.tar.bz2",
    "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
    "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
    "sherpa-onnx-moonshine-base-es-quantized-2026-02-27.tar.bz2",
    "silero_vad.onnx",
]

TEXTO_LECTURA = (
    "Por medio del presente escrito, la parte actora solicita que se tenga por desahogada "
    "la vista ordenada en el auto que antecede. Asimismo, manifiesta que la demandada no "
    "acreditó la personalidad de su apoderado legal, por lo que el amparo resulta procedente. "
    "Abre el navegador y busca la jurisprudencia sobre la suspensión del acto reclamado. "
    "Envía el contrato de arrendamiento al cliente y agenda una reunión para el jueves por la tarde. "
    "El servidor de base de datos se reinició después de actualizar el kernel de Linux, "
    "y el contenedor de Docker no levantó porque faltaba una variable de entorno. "
    "Recuérdame revisar los pendientes del expediente antes de las cinco."
)


# --------------------------------------------------------------------------- utilidades

def cargar_audio(ruta: Path) -> np.ndarray:
    """Cualquier formato que entienda ffmpeg → float32 mono 16 kHz."""
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(ruta), "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"],
        check=True, capture_output=True,
    ).stdout
    return np.frombuffer(out, dtype=np.float32).copy()


def palabras(texto: str) -> list[str]:
    texto = unicodedata.normalize("NFC", texto.lower())
    return re.findall(r"\w+", texto)


def distancia(a: list, b: list) -> int:
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


def wer_cer(ref: str, hip: str) -> tuple[float, float]:
    r, h = palabras(ref), palabras(hip)
    rc, hc = list(" ".join(r)), list(" ".join(h))
    return distancia(r, h) / max(len(r), 1), distancia(rc, hc) / max(len(rc), 1)


def pct(v: list[float], p: float) -> float:
    return float(np.percentile(v, p)) if v else 0.0


def rss_pico_mb() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024


def un_archivo(directorio: Path, patron: str) -> str:
    hallados = sorted(directorio.glob(patron))
    if not hallados:
        raise FileNotFoundError(f"{directorio}/{patron}")
    return str(hallados[0])


# --------------------------------------------------------------------------- modelos

def descubrir_modelos(dir_modelos: Path) -> list[dict]:
    modelos = []
    for d in sorted(dir_modelos.glob("*nemotron-3.5-asr-streaming*")):
        m = re.search(r"(\d+)ms", d.name)
        modelos.append({"clave": f"nemotron-{m.group(1) if m else '?'}ms", "tipo": "nemotron",
                        "dir": str(d), "bloque_ms": int(m.group(1)) if m else 0})
    for d in sorted(dir_modelos.glob("*parakeet-tdt-0.6b-v3*")):
        modelos.append({"clave": "parakeet-v3", "tipo": "parakeet", "dir": str(d)})
    for d in sorted(dir_modelos.glob("*moonshine-base-es*")):
        modelos.append({"clave": "moonshine-base-es", "tipo": "moonshine", "dir": str(d)})
    return [m for m in modelos if Path(m["dir"]).is_dir()]


def correr_nemotron(modelo: dict, audios: list[tuple[str, np.ndarray]], a: dict) -> dict:
    import sherpa_onnx

    d = Path(modelo["dir"])
    # El decodificador va un bloque atrás del audio: si el silencio de cierre no cubre
    # también el bloque, el endpoint se dispara antes de que salgan las últimas palabras.
    cierre = a["silencio"] + modelo["bloque_ms"] / 1000
    t0 = time.perf_counter()
    rec = sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=un_archivo(d, "tokens.txt"),
        encoder=un_archivo(d, "encoder*.onnx"),
        decoder=un_archivo(d, "decoder*.onnx"),
        joiner=un_archivo(d, "joiner*.onnx"),
        num_threads=a["hilos"],
        enable_endpoint_detection=True,
        rule1_min_trailing_silence=2.4,
        rule2_min_trailing_silence=cierre,
        rule3_min_utterance_length=30.0,
    )
    carga = time.perf_counter() - t0

    paso = int(SR * a["bloque_ms"] / 1000)
    computo, final_ms, textos, primer_parcial = [], [], {}, []
    total_audio = total_computo = 0.0
    for nombre, audio in audios:
        st = rec.create_stream()
        if a["idioma"]:
            st.set_option("language", a["idioma"])
        segmentos, visto_parcial = [], False
        audio = np.concatenate([audio, np.zeros(int(SR * (cierre + 1.0)), np.float32)])
        for i in range(0, len(audio), paso):
            st.accept_waveform(SR, audio[i:i + paso])
            t = time.perf_counter()
            while rec.is_ready(st):
                rec.decode_stream(st)
            dt_ = time.perf_counter() - t
            computo.append(dt_)
            total_computo += dt_
            texto = rec.get_result(st)
            if texto and not visto_parcial:
                visto_parcial = True
                primer_parcial.append((i + paso) / SR)
            if rec.is_endpoint(st):
                if texto.strip():
                    segmentos.append(texto.strip())
                    # fin de voz → texto final: cierre + cómputo del bloque que lo detectó
                    final_ms.append(cierre * 1000 + dt_ * 1000)
                rec.reset(st)
        st.input_finished()
        while rec.is_ready(st):
            rec.decode_stream(st)
        # lo que quedó tras el último endpoint (reset() deja el resultado vacío)
        resto = rec.get_result(st).strip()
        if resto:
            segmentos.append(resto)
        textos[nombre] = " ".join(segmentos)
        total_audio += len(audio) / SR

    bloque = modelo["bloque_ms"]
    return {
        "carga_s": carga,
        "rtf": total_computo / total_audio,
        "computo_p50_ms": pct(computo, 50) * 1000,
        "computo_p95_ms": pct(computo, 95) * 1000,
        "computo_max_ms": max(computo) * 1000 if computo else 0,
        "latencia_parcial_ms": bloque + pct(computo, 95) * 1000,
        "latencia_final_ms": float(np.mean(final_ms)) if final_ms else None,
        "primer_parcial_s": float(np.mean(primer_parcial)) if primer_parcial else None,
        "textos": textos,
    }


def correr_offline_vad(modelo: dict, audios: list[tuple[str, np.ndarray]], a: dict) -> dict:
    import sherpa_onnx

    d = Path(modelo["dir"])
    t0 = time.perf_counter()
    if modelo["tipo"] == "parakeet":
        rec = sherpa_onnx.OfflineRecognizer.from_transducer(
            tokens=un_archivo(d, "tokens.txt"),
            encoder=un_archivo(d, "encoder*.onnx"),
            decoder=un_archivo(d, "decoder*.onnx"),
            joiner=un_archivo(d, "joiner*.onnx"),
            num_threads=a["hilos"],
            model_type="nemo_transducer",
        )
    else:
        rec = sherpa_onnx.OfflineRecognizer.from_moonshine_v2(
            encoder=un_archivo(d, "encoder*.ort"),
            decoder=un_archivo(d, "decoder*.ort"),
            tokens=un_archivo(d, "tokens.txt"),
            num_threads=a["hilos"],
        )
    carga = time.perf_counter() - t0

    cfg = sherpa_onnx.VadModelConfig()
    cfg.silero_vad.model = a["vad"]
    cfg.silero_vad.min_silence_duration = a["silencio"]
    cfg.silero_vad.min_speech_duration = 0.25
    cfg.silero_vad.max_speech_duration = 20.0
    cfg.silero_vad.threshold = 0.5
    cfg.sample_rate = SR
    ventana = cfg.silero_vad.window_size

    decod, final_ms, textos = [], [], {}
    total_audio = total_computo = 0.0
    for nombre, audio in audios:
        vad = sherpa_onnx.VoiceActivityDetector(cfg, buffer_size_in_seconds=120)
        segmentos = []

        def vaciar():
            nonlocal total_computo
            while not vad.empty():
                muestras = vad.front.samples
                t = time.perf_counter()
                st = rec.create_stream()
                st.accept_waveform(SR, muestras)
                rec.decode_stream(st)
                dt_ = time.perf_counter() - t
                vad.pop()
                decod.append(dt_)
                total_computo += dt_
                final_ms.append(a["silencio"] * 1000 + dt_ * 1000)
                if st.result.text.strip():
                    segmentos.append(st.result.text.strip())

        for i in range(0, len(audio), ventana):
            t = time.perf_counter()
            vad.accept_waveform(audio[i:i + ventana])
            total_computo += time.perf_counter() - t
            vaciar()
        vad.flush()
        vaciar()
        textos[nombre] = " ".join(segmentos)
        total_audio += len(audio) / SR

    return {
        "carga_s": carga,
        "rtf": total_computo / total_audio,
        "computo_p50_ms": pct(decod, 50) * 1000,
        "computo_p95_ms": pct(decod, 95) * 1000,
        "computo_max_ms": max(decod) * 1000 if decod else 0,
        "latencia_parcial_ms": None,  # no hay parciales: el texto aparece al cerrar la frase
        "latencia_final_ms": float(np.mean(final_ms)) if final_ms else None,
        "primer_parcial_s": None,
        "segmentos": len(decod),
        "textos": textos,
    }


def trabajador(modelo: dict, rutas: list[str], a: dict) -> dict:
    audios = [(r, cargar_audio(Path(r))) for r in rutas]
    base = rss_pico_mb()
    fn = correr_nemotron if modelo["tipo"] == "nemotron" else correr_offline_vad
    res = fn(modelo, audios, a)
    res["ram_mb"] = rss_pico_mb() - base
    return res


# --------------------------------------------------------------------------- acciones

def descargar(dir_modelos: Path) -> None:
    dir_modelos.mkdir(parents=True, exist_ok=True)
    for nombre in DESCARGAS:
        destino = dir_modelos / nombre.removesuffix(".tar.bz2")
        if destino.exists():
            print(f"  ya existe {destino.name}")
            continue
        print(f"  bajando {nombre} …", flush=True)
        if nombre.endswith(".tar.bz2"):
            with urllib.request.urlopen(URL_BASE + nombre) as resp, tarfile.open(fileobj=resp, mode="r|bz2") as tar:
                tar.extractall(dir_modelos, filter="data")
        else:
            urllib.request.urlretrieve(URL_BASE + nombre, destino)


def grabar(carpeta: Path) -> Path:
    carpeta.mkdir(parents=True, exist_ok=True)
    ruta = carpeta / f"dictado-{dt.datetime.now():%Y%m%d-%H%M%S}.wav"
    print("\nLee en voz alta, a tu ritmo normal de dictado:\n")
    print("  " + TEXTO_LECTURA + "\n")
    input("Pulsa Enter para empezar a grabar…")
    proc = subprocess.Popen(["arecord", "-q", "-f", "S16_LE", "-r", str(SR), "-c", "1", str(ruta)])
    input("Grabando. Pulsa Enter al terminar…")
    proc.terminate()
    proc.wait()
    ruta.with_suffix(".txt").write_text(TEXTO_LECTURA + "\n", encoding="utf-8")
    print(f"Guardado {ruta} (+ referencia .txt)\n")
    return ruta


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("audios", nargs="*", help="archivos de audio; si existe <audio>.txt se usa como referencia")
    p.add_argument("--descargar", action="store_true", help="baja los modelos y termina")
    p.add_argument("--grabar", action="store_true", help="graba un dictado de referencia con arecord")
    p.add_argument("--modelos-dir", type=Path, default=DIR_MODELOS)
    p.add_argument("--solo", default="", help="filtra modelos por subcadena, separados por coma (p. ej. nemotron-160,parakeet)")
    p.add_argument("--hilos", type=int, default=min(4, os.cpu_count() or 4))
    p.add_argument("--idioma", default="es", help="idioma para Nemotron 3.5 ('es', 'auto', vacío = auto)")
    p.add_argument("--silencio", type=float, default=0.5, help="silencio (s) que cierra una frase")
    p.add_argument("--bloque-ms", type=int, default=80, help="cada cuánto llega audio del micrófono (ms)")
    p.add_argument("--json", type=Path, help="guarda resultados completos en este archivo")
    p.add_argument("--mostrar-texto", action="store_true", help="imprime las transcripciones")
    a = p.parse_args()

    if a.descargar:
        descargar(a.modelos_dir)
        return

    rutas = [str(Path(r).resolve()) for r in a.audios]
    if a.grabar:
        rutas.append(str(grabar(Path(__file__).parent / "grabaciones")))
    if not rutas:
        p.error("indica al menos un audio o usa --grabar")

    modelos = descubrir_modelos(a.modelos_dir)
    if a.solo:
        filtros = [f.strip() for f in a.solo.split(",") if f.strip()]
        modelos = [m for m in modelos if any(f in m["clave"] for f in filtros)]
    if not modelos:
        sys.exit(f"No hay modelos en {a.modelos_dir}. Corre primero: uv run stt_bench.py --descargar")

    opts = {"hilos": a.hilos, "idioma": a.idioma, "silencio": a.silencio, "bloque_ms": a.bloque_ms,
            "vad": str(a.modelos_dir / "silero_vad.onnx")}
    duracion = sum(len(cargar_audio(Path(r))) for r in rutas) / SR
    refs = {r: Path(r).with_suffix(".txt").read_text(encoding="utf-8") for r in rutas
            if Path(r).with_suffix(".txt").exists()}
    print(f"{len(rutas)} audio(s), {duracion:.1f} s · {a.hilos} hilos · silencio de cierre {a.silencio}s"
          f" · {len(refs)} con referencia\n")

    resultados = {}
    for m in modelos:
        print(f"→ {m['clave']} …", flush=True)
        # cada modelo en su propio proceso: la RAM medida es solo la suya
        with cf.ProcessPoolExecutor(1, mp_context=mp.get_context("spawn")) as ex:
            res = ex.submit(trabajador, m, rutas, opts).result()
        if refs:
            w = [wer_cer(refs[r], res["textos"].get(r, "")) for r in refs]
            res["wer"] = float(np.mean([x[0] for x in w]))
            res["cer"] = float(np.mean([x[1] for x in w]))
        resultados[m["clave"]] = res

    f = lambda v, fmt: "—" if v is None else format(v, fmt)
    cab = f"{'modelo':<20}{'carga s':>8}{'RAM MB':>8}{'RTF':>7}{'cómputo p50/p95 ms':>20}{'parcial ms':>11}{'final ms':>10}{'WER':>7}{'CER':>7}"
    print("\n" + cab + "\n" + "─" * len(cab))
    for k, r in resultados.items():
        print(f"{k:<20}{r['carga_s']:>8.1f}{r['ram_mb']:>8.0f}{r['rtf']:>7.3f}"
              f"{r['computo_p50_ms']:>11.0f} / {r['computo_p95_ms']:<6.0f}"
              f"{f(r['latencia_parcial_ms'], '.0f'):>11}{f(r['latencia_final_ms'], '.0f'):>10}"
              f"{f(r.get('wer'), '.1%'):>7}{f(r.get('cer'), '.1%'):>7}")
    print("\nparcial = bloque del modelo + cómputo p95 (cuánto tarda en aparecer texto mientras hablas).")
    print("final   = silencio de cierre (+ un bloque en Nemotron) + cómputo: de que dejas de hablar a texto definitivo.")
    print("RTF < 1 = más rápido que tiempo real. En Nemotron el cómputo es por bloque de "
          f"{a.bloque_ms} ms; en los offline, por frase.")

    if a.mostrar_texto or not refs:
        for r in rutas:
            print(f"\n=== {Path(r).name}")
            if r in refs:
                print(f"  {'referencia':<20}{refs[r].strip()}")
            for k, res in resultados.items():
                print(f"  {k:<20}{res['textos'].get(r, '')}")

    if a.json:
        a.json.write_text(json.dumps({"opciones": opts, "audios": rutas, "resultados": resultados},
                                     ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"\nResultados en {a.json}")


if __name__ == "__main__":
    main()
