# /// script
# requires-python = ">=3.10,<3.14"
# dependencies = ["onnx", "sherpa-onnx==1.13.8", "numpy", "soundfile"]
# ///
"""Empaqueta una voz entrenada con Piper para que la cargue sherpa-onnx (y la app).

Toma el `.onnx` y el `.onnx.json` que exporta `bench/corpus/entrenar_piper.ipynb` y arma
`~/.local/share/iurevoz/voces/vits-piper-es_MX-<nombre>-medium/` como los paquetes oficiales de
sherpa: metadatos en el ONNX (igual que scripts/piper/add_meta_data.py de sherpa-onnx v1.13.8),
`tokens.txt` desde `phoneme_id_map` y `espeak-ng-data` copiado de una voz Piper ya instalada.
Al final sintetiza una frase de prueba.

Uso:
  uv run empaquetar_voz.py descargas/es_MX-iurevoz-medium.onnx
"""

import argparse
import json
import shutil
from pathlib import Path

import onnx

VOCES = Path(__import__("os").environ.get("IUREVOZ_VOCES", Path.home() / ".local/share/iurevoz/voces"))
BUSCAR_ESPEAK = [VOCES, Path.home() / ".cache/stt-bench/tts"]


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("onnx", type=Path, help="es_MX-<nombre>-medium.onnx (con su .onnx.json al lado)")
    p.add_argument("--prueba", default="Hola, ¿cómo estás? Llámale a Guillermo para que traiga la llave del archivo.")
    a = p.parse_args()

    config = json.loads(a.onnx.with_suffix(".onnx.json").read_text(encoding="utf-8"))
    base = a.onnx.name.removesuffix(".onnx")
    destino = VOCES / f"vits-piper-{base}"
    destino.mkdir(parents=True, exist_ok=True)

    modelo = onnx.load(a.onnx)
    del modelo.metadata_props[:]
    meta = {
        "model_type": "vits", "comment": "piper", "language": "Spanish",
        "voice": config["espeak"]["voice"], "version": 1, "has_espeak": 1, "has_g2pw": 0,
        "n_speakers": config["num_speakers"], "sample_rate": config["audio"]["sample_rate"],
    }
    for k, v in meta.items():
        m = modelo.metadata_props.add()
        m.key, m.value = k, str(v)
    onnx.save(modelo, destino / a.onnx.name)
    shutil.copy(a.onnx.with_suffix(".onnx.json"), destino / f"{a.onnx.name}.json")

    # sherpa-onnx 1.13.8 solo acepta tokens de un carácter. Piper 1.8 agrega al mapa grupos de
    # vocales («aɪ», «oʊ») que solo se usan si se entrena con --data.vowel_clusters (aquí no).
    with (destino / "tokens.txt").open("w", encoding="utf-8") as f:
        for s, i in config["phoneme_id_map"].items():
            if s != "\n" and len(s) == 1:
                f.write(f"{s} {i[0] if isinstance(i, list) else i}\n")

    if not (destino / "espeak-ng-data").exists():
        fuente = next((d / "espeak-ng-data" for b in BUSCAR_ESPEAK if b.exists() for d in b.glob("vits-piper-*") if (d / "espeak-ng-data").exists() and d != destino), None)
        if fuente is None:
            raise SystemExit("no encontré espeak-ng-data: instala antes alguna voz Piper desde la app")
        shutil.copytree(fuente, destino / "espeak-ng-data")

    import sherpa_onnx as so
    import numpy as np
    import soundfile as sf
    tts = so.OfflineTts(so.OfflineTtsConfig(model=so.OfflineTtsModelConfig(vits=so.OfflineTtsVitsModelConfig(
        model=str(destino / a.onnx.name), tokens=str(destino / "tokens.txt"), data_dir=str(destino / "espeak-ng-data")), num_threads=2)))
    au = tts.generate(a.prueba, so.GenerationConfig())
    sf.write(destino / "prueba.wav", np.asarray(au.samples, dtype=np.float32), au.sample_rate)
    print(f"listo: {destino}\nprueba: {destino / 'prueba.wav'}\n"
          f'para la app, agrega a VOCES en src/voz.rs: ("piper-{config.get("dataset", base.split("-")[1])}", "vits-piper-{base}", "…"),')


if __name__ == "__main__":
    main()
