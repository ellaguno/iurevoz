"""Filtro fonético del corpus sintético: compara lo que dijo el maestro con lo que espera espeak.

Un reconocedor de fonemas (wav2vec2-xlsr-53-espeak-cv-ft, Apache-2.0, alfabeto de espeak)
transcribe el audio. Su salida se compara con la fonemización de espeak-ng es-419, la misma que
usa Piper. Ambas se reducen a clases gruesas para no castigar alófonos (β/b, ð/d, ɾ/r).

Detecta lo que el ASR de palabras no ve: «lámale» por «llámale» (Y→l), sílabas comidas o
repetidas. La zeta de España (θ) no se puede juzgar: el reconocedor también la «oye» en la voz
mexicana real del usuario, así que se trata como «s».

Calibración (2026-10-21): en la voz real del usuario el PER sale entre 0 y 7 %, y en Qwen3
(variantes A y C) entre 0 y 10 %. La variante B, que dice «lámale», se marca como ll→l.

Uso como módulo (cuaderno de generación) o desde la terminal:
  python filtro_fonemas.py carpeta_con_00.wav… "frase 0" "frase 1"…
"""

import re
import sys
from functools import lru_cache

import numpy as np

MODELO = "facebook/wav2vec2-xlsr-53-espeak-cv-ft"

# clase gruesa de cada símbolo; lo que no aparece se ignora (marcas, tonos, longitudes)
CLASES = {
    **dict.fromkeys(["a", "ɐ", "ɑ", "æ", "ä"], "a"),
    **dict.fromkeys(["e", "ɛ", "e̞", "ə"], "e"),
    **dict.fromkeys(["i", "ɪ", "ɨ", "y"], "i"),
    **dict.fromkeys(["o", "ɔ", "o̞"], "o"),
    **dict.fromkeys(["u", "ʊ", "w"], "u"),
    **dict.fromkeys(["b", "β", "v", "ʋ"], "b"),
    **dict.fromkeys(["d", "ð"], "d"),
    **dict.fromkeys(["g", "ɡ", "ɣ"], "g"),
    **dict.fromkeys(["ʝ", "ʎ", "j", "ʒ", "ɟ"], "Y"),
    **dict.fromkeys(["r", "ɾ", "ɹ", "ʁ", "ɽ"], "r"),
    **dict.fromkeys(["s", "z", "ʃ"], "s"),
    **dict.fromkeys(["x", "h", "χ"], "x"),
    **dict.fromkeys(["n", "ŋ", "ɲ", "m"], None),  # se resuelven abajo
    "p": "p", "t": "t", "k": "k", "f": "f", "l": "l", "θ": "s", "c": "k", "q": "k",
}
NASALES = {"n": "n", "ŋ": "n", "ɲ": "ɲ", "m": "m"}
DIGRAFOS = {"tʃ": "C", "dʒ": "Y", "ts": "C"}


def clases(fonemas: str) -> str:
    s = re.sub(r"[ˈˌː0-9.\[\]^\"' ,]", "", fonemas)
    for a, b in DIGRAFOS.items():
        s = s.replace(a, b)
    out = []
    for ch in s:
        if ch in ("C", "Y"):
            out.append(ch)
        elif ch in NASALES:
            out.append(NASALES[ch])
        elif CLASES.get(ch):
            out.append(CLASES[ch])
    return re.sub(r"(.)\1+", r"\1", "".join(out))  # «ʝʝ», «rr»: la duración no se juzga


@lru_cache(maxsize=1)
def _modelo(dispositivo: str = "cpu"):
    import json
    import torch
    from huggingface_hub import hf_hub_download
    from transformers import Wav2Vec2ForCTC
    m = Wav2Vec2ForCTC.from_pretrained(MODELO).to(dispositivo).eval()
    vocab = json.load(open(hf_hub_download(MODELO, "vocab.json")))
    return m, {v: k for k, v in vocab.items()}, torch


def reconocer(audio16k: np.ndarray, dispositivo: str = "cpu") -> str:
    m, inv, torch = _modelo(dispositivo)
    x = torch.from_numpy((audio16k - audio16k.mean()) / (audio16k.std() + 1e-7)).float()[None].to(dispositivo)
    with torch.no_grad():
        ids = m(x).logits.argmax(-1)[0].tolist()
    out, prev = [], None
    for i in ids:
        if i != prev and inv[i] not in ("<pad>", "<s>", "</s>", "<unk>"):
            out.append(inv[i])
        prev = i
    return " ".join(out)


def datos_espeak():
    """Carpeta de datos de espeak de piper_phonemize. espeak ignora rutas de más de ~160
    caracteres (y cae a /usr/share sin avisar), así que se enlaza a una ruta corta si hace falta."""
    import os
    import tempfile
    from pathlib import Path
    import piper_phonemize
    datos = Path(piper_phonemize.__file__).parent / "espeak-ng-data"
    if len(str(datos)) < 150:
        return datos
    corto = Path(tempfile.gettempdir()) / f"espeak-ng-data-{os.getuid()}"
    if not corto.exists():
        corto.symlink_to(datos)
    return corto


def esperado(texto: str) -> str:
    """Fonemas de espeak-ng es-419: con piper_phonemize si está (solo hay paquetes hasta
    Python 3.11) o, si no, con el comando `espeak-ng` del sistema (apt install espeak-ng)."""
    try:
        from piper_phonemize import phonemize_espeak
    except ImportError:
        import subprocess
        r = subprocess.run(["espeak-ng", "-q", "--ipa", "-v", "es-419", texto], capture_output=True, text=True, check=True)
        return " ".join(r.stdout.split())
    return " ".join("".join(o) for o in phonemize_espeak(texto, "es-419", data_path=datos_espeak()))


def distancia(a: str, b: str) -> int:
    d = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        prev, d[0] = d[0], i
        for j, cb in enumerate(b, 1):
            prev, d[j] = d[j], min(d[j] + 1, d[j - 1] + 1, prev + (ca != cb))
    return d[-1]


UMBRAL_PER = 0.15


def aceptable(r: dict) -> bool:
    return r["per"] <= UMBRAL_PER and not r["avisos"]


def evaluar(texto: str, audio16k: np.ndarray, dispositivo: str = "cpu") -> dict:
    """PER en clases gruesas y aviso de «ll» pronunciada como «l»."""
    e, r = clases(esperado(texto)), clases(reconocer(audio16k, dispositivo))
    avisos = []
    if e.count("Y") > r.count("Y") and r.count("l") > e.count("l"):
        avisos.append("ll→l")
    return {"per": distancia(e, r) / max(1, len(e)), "avisos": avisos, "esperado": e, "reconocido": r}


if __name__ == "__main__":
    from pathlib import Path
    import soundfile as sf
    from scipy.signal import resample_poly
    carpeta, frases = Path(sys.argv[1]), sys.argv[2:]
    for i, t in enumerate(frases):
        x, sr = sf.read(carpeta / f"{i:02d}.wav", dtype="float32", always_2d=True)
        r = evaluar(t, resample_poly(x[:, 0], 16000, sr).astype(np.float32))
        print(f"{i:02d} per={r['per']:.2f} {' '.join(r['avisos']) or '-':8} {t}\n   esp {r['esperado']}\n   rec {r['reconocido']}")
