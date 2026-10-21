# /// script
# requires-python = ">=3.10,<3.12"
# dependencies = ["piper-phonemize"]
# ///
"""Arma el guion del corpus para entrenar la voz propia (fase T3 de docs/plan-tts-propio.md).

Mezcla tres fuentes:
  1. frases de dominio (corpus/frases_dominio.txt): entran todas
  2. plantillas con montos, fechas, horas y teléfonos, normalizadas con `iurevoz normalizar`
     para que el texto sea exactamente lo que la voz oirá en la app
  3. frases de Common Voice en español (CC0), elegidas con un greedy por cobertura de
     difonos (fonemas de espeak-ng es-419, los mismos que usa Piper)

Quita las frases de evaluación (frases_tts.tsv), las ya grabadas (guion_voz.txt) y las
típicas de España (vosotros, «vale», «coger»…).

Uso:
  uv run corpus_tts.py                  # 6000 frases → corpus/guion_corpus.tsv
  uv run corpus_tts.py --n 3000 --semilla 2
"""

import argparse
import heapq
import math
import random
import re
import subprocess
import urllib.request
from collections import Counter
from pathlib import Path

import piper_phonemize
from piper_phonemize import phonemize_espeak

AQUI = Path(__file__).parent
ESPEAK = Path(piper_phonemize.__file__).parent / "espeak-ng-data"
CACHE = Path.home() / ".cache/stt-bench/textos"
URL_CV = "https://raw.githubusercontent.com/common-voice/common-voice/main/server/data/es/"
FUENTES_CV = ["sentence-collector.txt", "wiki.es.txt"]

# España y otras variantes que no queremos en la voz mexicana
EXCLUIR = re.compile(
    r"\b(vosotr[oa]s|os|vuestr[oa]s?|sois|habéis|vale|coger|cog[eií]\w*|ordenador|coche|zumo|móvil|guay|tío|tía|"
    r"gilipollas|chaval\w*|mola|vais|estáis|tenéis|queréis|podéis|sabéis|\w+áis|\w+éis)\b",
    re.IGNORECASE,
)
PERMITIDO = re.compile(r"^[A-Za-zÁÉÍÓÚÜÑáéíóúüñ¿¡ ,.;?!]+$")
MESES = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"]


def descargar_cv() -> dict[str, list[str]]:
    CACHE.mkdir(parents=True, exist_ok=True)
    fuentes = {}
    for f in FUENTES_CV:
        ruta = CACHE / f
        if not ruta.exists():
            print(f"descargando {f}…")
            urllib.request.urlretrieve(URL_CV + f, ruta)
        fuentes[f] = ruta.read_text(encoding="utf-8").splitlines()
    return fuentes


def palabras(t: str) -> list[str]:
    return re.findall(r"[a-záéíóúüñ]+", t.lower())


def limpiar(t: str) -> str | None:
    t = " ".join(t.replace(" ?", "?").replace(" !", "!").replace("¿ ", "¿").replace("¡ ", "¡").split())
    if not PERMITIDO.match(t) or EXCLUIR.search(t):
        return None
    palabras = t.split()
    if not 5 <= len(palabras) <= 22:
        return None
    # nombres propios a media oración o palabras con letras raras en español: fuera
    if any(p[0].isupper() and not prev.endswith((".", "?", "!")) for prev, p in zip(palabras, palabras[1:])) or any(re.search(r"[wk]|sh|th|ck|sz|tz|zz|gg|ff|ss|pp|tt|dd|bb|mm|^ll$", p.lower()) for p in palabras):
        return None
    # mayúscula al inicio y tras cada ¿ o ¡ que abre oración
    t = re.sub(r"(^|[.?!] )([¿¡]?)([a-záéíóúñ])", lambda m: m.group(1) + m.group(2) + m.group(3).upper(), t)
    t = re.sub(r"(, [¿¡])([A-ZÁÉÍÓÚÑ])", lambda m: m.group(1) + m.group(2).lower(), t)
    if t[-1] not in ".?!":
        t += "."
    if t.count("?") != t.count("¿") or t.count("!") != t.count("¡"):
        return None
    return t


def plantillas(rng: random.Random, n: int) -> list[str]:
    """Frases con cifras, como las escribiría el asistente; luego se normalizan."""
    nombres = ["Pérez", "Ramírez", "Hernández", "García", "López", "Martínez", "Juárez", "Gómez", "Torres", "Flores"]
    def monto():
        return f"${rng.choice([rng.randint(100, 999), rng.randint(1000, 99999), rng.randint(100, 999) * 1000]):,}" + rng.choice(["", ".50", ".00"])
    def fecha():
        return f"{rng.randint(1, 28)} de {rng.choice(MESES)} de {rng.randint(2024, 2029)}"
    def hora():
        return f"{rng.randint(8, 19)}:{rng.choice(['00', '15', '30', '45'])}"
    def tel():
        return f"55 {rng.randint(1000, 9999)} {rng.randint(1000, 9999)}"
    formas = [
        lambda: f"El total a pagar es de {monto()} más IVA.",
        lambda: f"La pensión quedó en {monto()} mensuales.",
        lambda: f"El cliente debe {monto()} de rentas vencidas.",
        lambda: f"Se condenó al pago de {monto()} por daños y perjuicios.",
        lambda: f"Los honorarios son {monto()} por la primera etapa.",
        lambda: f"El plazo vence el {fecha()}.",
        lambda: f"La audiencia quedó para el {fecha()} a las {hora()}.",
        lambda: f"La cita con el licenciado {rng.choice(nombres)} es a las {hora()}.",
        lambda: f"Mañana a las {hora()} tienes junta con la licenciada {rng.choice(nombres)}.",
        lambda: f"El expediente {rng.randint(1, 999)}/{rng.randint(2020, 2026)} está en la sala {rng.randint(1, 9)}.",
        lambda: f"Según el art. {rng.randint(1, 140)} de la ley, el plazo es de {rng.randint(3, 30)} días.",
        lambda: f"Revisa la fracción {rng.choice(['I', 'II', 'III', 'IV', 'V', 'VI', 'VII', 'VIII', 'IX', 'X'])} del art. {rng.randint(1, 140)}.",
        lambda: f"El descuento es del {rng.randint(5, 50)} % sobre el total.",
        lambda: f"Llámale al licenciado {rng.choice(nombres)} al {tel()}.",
        lambda: f"Registra {rng.randint(1, 9)} horas en el asunto de {rng.choice(nombres)}.",
        lambda: f"Llevamos {rng.randint(2, 40)} audiencias en {rng.randint(2, 12)} meses.",
        lambda: f"El documento tiene {rng.randint(2, 300)} páginas y {rng.randint(1, 20)} anexos.",
        lambda: f"Tienes {rng.randint(2, 15)} pendientes y {rng.randint(1, 5)} vencimientos esta semana.",
    ]
    return [rng.choice(formas)() for _ in range(n)]


def normalizar(frases: list[str], binario: Path) -> list[str]:
    r = subprocess.run([str(binario), "normalizar"], input="\n".join(frases), capture_output=True, text=True, check=True)
    return r.stdout.splitlines()


def fonemas(t: str) -> list[str]:
    """Fonemas de espeak es-419 sin acentos de intensidad, con «#» como límite de palabra."""
    out = []
    for oracion in phonemize_espeak(t, "es-419", data_path=ESPEAK):
        for f in oracion:
            if f in "ˈˌ":
                continue
            out.append("#" if f == " " else f)
    return [f for f in out if f not in ",.;:?!¿¡"]


def difonos(fs: list[str]) -> Counter:
    fs = ["#"] + fs + ["#"]
    return Counter(zip(fs, fs[1:]))


def seleccionar(candidatas: list[tuple[str, Counter, float]], n: int, cubierto: Counter) -> list[int]:
    """Greedy perezoso: cada difono vale 1/(1+veces ya elegido), normalizado por longitud y
    multiplicado por el peso de la fuente. Actualiza `cubierto`."""
    def valor(i):
        _, d, peso = candidatas[i]
        tot = sum(d.values())
        return sum(c / (1 + cubierto[k]) for k, c in d.items()) / math.sqrt(tot) * peso
    heap = [(-valor(i), i) for i in range(len(candidatas))]
    heapq.heapify(heap)
    elegidas = []
    while heap and len(elegidas) < n:
        v, i = heapq.heappop(heap)
        nv = valor(i)
        if heap and nv < -heap[0][0] - 1e-9:
            heapq.heappush(heap, (-nv, i))
            continue
        elegidas.append(i)
        cubierto.update(candidatas[i][1])
    return elegidas


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--n", type=int, default=6000, help="frases en total")
    p.add_argument("--plantillas", type=int, default=500)
    p.add_argument("--candidatas", type=int, default=150000, help="frases de Common Voice a considerar (al azar)")
    p.add_argument("--preguntas", type=float, default=0.12, help="fracción de preguntas y exclamaciones")
    p.add_argument("--min-frecuencia", type=int, default=5, help="cada palabra debe aparecer al menos tantas veces en Common Voice")
    p.add_argument("--semilla", type=int, default=1)
    p.add_argument("--binario", type=Path, default=AQUI.parent / "target/release/iurevoz")
    p.add_argument("--salida", type=Path, default=AQUI / "corpus/guion_corpus.tsv")
    a = p.parse_args()
    rng = random.Random(a.semilla)

    excluir = {l.split("\t")[0].strip().lower() for l in (AQUI / "frases_tts.tsv").read_text().splitlines() if l and not l.startswith("#")}
    excluir |= {l.strip().lower() for l in (AQUI / "guion_voz.txt").read_text().splitlines() if l and not l.startswith("#")}

    dominio = [l.strip() for l in (AQUI / "corpus/frases_dominio.txt").read_text().splitlines() if l.strip() and not l.startswith("#")]
    cifras = normalizar(plantillas(rng, a.plantillas), a.binario)
    fijas = list(dict.fromkeys(f for f in dominio + cifras if f.lower() not in excluir))
    origen = {f: "dominio" for f in dominio} | {f: "plantilla" for f in cifras}

    fuentes = descargar_cv()
    frecuencia = Counter(w for lineas in fuentes.values() for l in lineas for w in palabras(l))
    peso_fuente = {"sentence-collector.txt": 1.3, "wiki.es.txt": 1.0}  # las del colector son conversacionales
    cv: dict[str, float] = {}
    for f, lineas in fuentes.items():
        for t in filter(None, map(limpiar, lineas)):
            if t.lower() not in excluir and t not in origen and all(frecuencia[w] >= a.min_frecuencia for w in palabras(t)):
                cv.setdefault(t, peso_fuente[f])
    textos = list(cv)
    rng.shuffle(textos)
    preg = [t for t in textos if t[-1] in "?!"]
    resto = [t for t in textos if t[-1] not in "?!"][: a.candidatas]
    print(f"{len(fijas)} frases fijas; candidatas de Common Voice: {len(preg)} preguntas/exclamaciones y {len(resto)} afirmaciones")

    cubierto = Counter()
    for f in fijas:
        cubierto.update(difonos(fonemas(f)))
    universo = set(cubierto)
    def candidatas(ts):
        c = [(t, difonos(fonemas(t)), cv[t]) for t in ts]
        for _, d, _ in c:
            universo.update(d)
        return c
    cp, cr = candidatas(preg), candidatas(resto)
    faltan_preg = max(0, int(a.n * a.preguntas) - sum(f[-1] in "?!" for f in fijas))
    elegidas = [cp[i][0] for i in seleccionar(cp, faltan_preg, cubierto)]
    elegidas += [cr[i][0] for i in seleccionar(cr, max(0, a.n - len(fijas) - len(elegidas)), cubierto)]

    final = fijas + elegidas
    cubiertos = Counter()
    for f in final:
        cubiertos.update(difonos(fonemas(f)))
    pocos = sum(1 for d in universo if cubiertos[d] < 3)
    caracteres = sum(len(f) for f in final)
    a.salida.parent.mkdir(parents=True, exist_ok=True)
    with a.salida.open("w", encoding="utf-8") as fh:
        fh.write("# id\ttexto\torigen (dominio | plantilla | commonvoice, CC0)\n")
        for i, f in enumerate(final, 1):
            fh.write(f"{i:05d}\t{f}\t{origen.get(f, 'commonvoice')}\n")
    preguntas = sum(f.endswith(("?", "!")) for f in final)
    print(f"{len(final)} frases → {a.salida}")
    print(f"difonos cubiertos: {len(set(cubiertos) & universo)}/{len(universo)} ({pocos} con menos de 3 apariciones)")
    print(f"preguntas/exclamaciones: {preguntas} ({100 * preguntas / len(final):.0f} %)")
    print(f"≈ {caracteres / 14.5 / 3600:.1f} h de audio (a ~14.5 caracteres por segundo)")


if __name__ == "__main__":
    main()
