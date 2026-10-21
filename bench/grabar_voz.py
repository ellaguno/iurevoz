# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "soundfile"]
# ///
"""Graba la voz propia frase por frase para clonarla y entrenar el TTS.

Muestra cada frase del guion, graba con arecord (48 kHz, 24 bits, mono) y guarda
NNN.wav + metadata.csv (formato LJSpeech: `NNN|texto`, el que usa el entrenamiento de Piper).
Retoma donde se quedó: las frases ya grabadas se saltan.

Uso:
  uv run grabar_voz.py                          # guion_voz.txt → ~/.local/share/iurevoz/mi-voz
  uv run grabar_voz.py --dispositivo hw:1,0     # otro micrófono (ver `arecord -l`)

Teclas en cada frase: Enter = empezar; Enter = terminar; luego Enter = guardar,
r = repetir, s = saltar, q = salir.
"""

import argparse
import subprocess
import time
from pathlib import Path

import numpy as np
import soundfile as sf

AQUI = Path(__file__).parent


def frases(ruta: Path) -> list[str]:
    return [l.strip() for l in ruta.read_text(encoding="utf-8").splitlines() if l.strip() and not l.startswith("#")]


def revisar(ruta: Path) -> str:
    x, sr = sf.read(ruta, always_2d=True)
    x = x[:, 0]
    pico = float(np.abs(x).max()) if len(x) else 0.0
    avisos = [f"{len(x) / sr:.1f} s", f"pico {20 * np.log10(max(pico, 1e-9)):.0f} dBFS"]
    if pico > 0.97:
        avisos.append("⚠ SATURADO: aléjate un poco o baja la ganancia")
    elif pico < 0.1:
        avisos.append("⚠ muy bajo: acércate o sube la ganancia")
    # ruido de fondo: el 10 % de ventanas de 50 ms más silenciosas
    v = int(0.05 * sr)
    if len(x) > 4 * v:
        rms = np.sqrt(np.mean(x[: len(x) // v * v].reshape(-1, v) ** 2, axis=1))
        piso = 20 * np.log10(max(float(np.percentile(rms, 10)), 1e-9))
        avisos.append(f"fondo {piso:.0f} dBFS" + (" ⚠ ruidoso" if piso > -50 else ""))
    return ", ".join(avisos)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--guion", type=Path, default=AQUI / "guion_voz.txt")
    p.add_argument("--destino", type=Path, default=Path.home() / ".local/share/iurevoz/mi-voz")
    p.add_argument("--dispositivo", default="", help="dispositivo ALSA para arecord (vacío = el predeterminado)")
    a = p.parse_args()

    a.destino.mkdir(parents=True, exist_ok=True)
    meta = a.destino / "metadata.csv"
    hechas = {l.split("|", 1)[0] for l in meta.read_text(encoding="utf-8").splitlines()} if meta.exists() else set()
    lista = frases(a.guion)
    print(f"{len(lista)} frases, {len(hechas)} ya grabadas. Destino: {a.destino}\n"
          "Deja medio segundo de silencio antes y después de cada frase.\n")
    for i, texto in enumerate(lista, 1):
        nid = f"{i:03d}"
        if nid in hechas:
            continue
        while True:
            print(f"[{i}/{len(lista)}]  {texto}")
            op = input("  Enter para grabar (s = saltar, q = salir) ").strip().lower()
            if op == "q":
                return
            if op == "s":
                break
            ruta = a.destino / f"{nid}.wav"
            cmd = ["arecord", "-q", "-f", "S24_3LE", "-r", "48000", "-c", "1", str(ruta)]
            if a.dispositivo:
                cmd[2:2] = ["-D", a.dispositivo]
            proc = subprocess.Popen(cmd)
            time.sleep(0.1)
            input("  ● grabando… Enter para terminar ")
            time.sleep(0.4)  # que no se corte el final
            proc.terminate()
            proc.wait()
            print("  " + revisar(ruta))
            r = input("  Enter = guardar, r = repetir, q = salir ").strip().lower()
            if r == "r":
                continue
            if r == "q":
                ruta.unlink(missing_ok=True)
                return
            with meta.open("a", encoding="utf-8") as f:
                f.write(f"{nid}|{texto}\n")
            break
    print(f"\nListo: {len(frases(a.guion))} frases en el guion; grabaciones en {a.destino}")


if __name__ == "__main__":
    main()
