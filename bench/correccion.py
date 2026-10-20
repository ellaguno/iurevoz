# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Benchmark de corrección de dictado con LLM (etapa 2 de iurevoz).

Toma salidas reales del ASR (resultados.json de stt_bench.py) y su referencia, y le pide a
cada modelo que corrija puntuación, mayúsculas y palabras mal oídas. Mide latencia y, sobre
todo, cuántas palabras arregla y cuántas rompe (correcciones plausibles pero inventadas).

Modelos: ollama:<modelo> (local, API nativa) u openrouter:<modelo> (requiere OPENROUTER_API_KEY).

Uso:
  uv run correccion.py ollama:gemma4:e2b ollama:qwen3:1.7b
  uv run correccion.py openrouter:google/gemma-3-4b-it --mostrar
  uv run correccion.py ollama:qwen3:4b --glosario glosario.txt --json correccion.json
"""

import argparse
import difflib
import json
import os
import re
import statistics
import sys
import time
import unicodedata
import urllib.error
import urllib.request
from pathlib import Path

AQUI = Path(__file__).parent
OLLAMA = os.environ.get("OLLAMA_HOST", "http://localhost:11434").rstrip("/")
OPENROUTER = "https://openrouter.ai/api/v1/chat/completions"

SISTEMA = (
    "Eres un corrector de dictado en español (México). Recibes texto de un reconocedor de voz. "
    "Corrige SOLO puntuación, mayúsculas y palabras claramente mal reconocidas por sonido parecido. "
    "No cambies el estilo, no resumas, no agregues ni quites ideas. "
    "Si no estás seguro de una palabra, déjala como está. "
    "Escribe los números con letra, como se dictaron. "
    "Devuelve únicamente el texto corregido, sin comillas ni comentarios."
)


# ── métricas ──────────────────────────────────────────────────────────────────

def palabras(texto: str) -> list[str]:
    """Igual que stt_bench: minúsculas, sin puntuación, con acentos."""
    return re.findall(r"\w+", unicodedata.normalize("NFC", texto.lower()))


def fichas(texto: str) -> list[str]:
    """Palabras con mayúsculas y signos como fichas aparte: mide el formato."""
    return re.findall(r"\w+|[^\w\s]", unicodedata.normalize("NFC", texto))


def tasa_error(ref: list[str], hip: list[str]) -> float:
    m = difflib.SequenceMatcher(None, ref, hip, autojunk=False)
    errores = sum(max(i2 - i1, j2 - j1) for op, i1, i2, j1, j2 in m.get_opcodes() if op != "equal")
    return errores / max(len(ref), 1)


def aciertos(ref: list[str], hip: list[str]) -> set[int]:
    """Índices de la referencia que la hipótesis reproduce bien."""
    m = difflib.SequenceMatcher(None, ref, hip, autojunk=False)
    return {b.a + k for b in m.get_matching_blocks() for k in range(b.size)}


def cambios(antes: list[str], despues: list[str]) -> list[tuple[str, str]]:
    """Palabras que el LLM tocó respecto a la entrada (lo que la salvaguarda resaltaría)."""
    m = difflib.SequenceMatcher(None, antes, despues, autojunk=False)
    return [(" ".join(antes[i1:i2]), " ".join(despues[j1:j2]))
            for op, i1, i2, j1, j2 in m.get_opcodes() if op != "equal"]


# ── casos ─────────────────────────────────────────────────────────────────────

def sin_formato(texto: str) -> str:
    return " ".join(palabras(texto))


def cargar_casos(ruta_resultados: Path) -> list[dict]:
    datos = json.loads(ruta_resultados.read_text(encoding="utf-8"))
    casos = []
    for audio in datos["audios"]:
        ref_txt = Path(audio).with_suffix(".txt")
        if not ref_txt.exists():
            continue
        ref = ref_txt.read_text(encoding="utf-8").strip()
        nombre = Path(audio).stem
        for modelo, r in datos["resultados"].items():
            hip = r.get("textos", {}).get(audio)
            if hip:
                casos.append({"id": f"{nombre}/{modelo}", "entrada": hip.strip(), "ref": ref})
        # Solo puntuación: el texto correcto sin signos ni mayúsculas, completo y por frase.
        casos.append({"id": f"{nombre}/sin-formato", "entrada": sin_formato(ref), "ref": ref})
        for i, frase in enumerate(re.split(r"(?<=\.)\s+", ref), 1):
            casos.append({"id": f"{nombre}/frase-{i}", "entrada": sin_formato(frase), "ref": frase})
    return casos


# ── backends ──────────────────────────────────────────────────────────────────

def post(url: str, cuerpo: dict, cabeceras: dict | None = None, espera: float = 120) -> dict:
    req = urllib.request.Request(url, json.dumps(cuerpo).encode(),
                                 {"Content-Type": "application/json", **(cabeceras or {})})
    for intento in range(4):
        try:
            with urllib.request.urlopen(req, timeout=espera) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            # límite de peticiones o proveedor caído: reintenta con espera creciente
            if e.code not in (429, 500, 502, 503, 504) or intento == 3:
                raise
            time.sleep(2 * 2 ** intento)


def tope_salida(texto: str) -> int:
    """La corrección mide lo mismo que la entrada; el tope corta modelos que divagan o razonan."""
    return 2 * len(texto) // 3 + 64


def llamar_ollama(modelo: str, sistema: str, texto: str) -> dict:
    cuerpo = {"model": modelo, "stream": False, "think": False, "keep_alive": "10m",
              "options": {"temperature": 0, "num_ctx": 4096, "num_predict": tope_salida(texto)},
              "messages": [{"role": "system", "content": sistema}, {"role": "user", "content": texto}]}
    t = time.perf_counter()
    try:
        r = post(f"{OLLAMA}/api/chat", cuerpo)
    except urllib.error.HTTPError as e:
        if e.code != 400:
            raise
        cuerpo.pop("think")  # modelos sin modo de razonamiento rechazan el parámetro
        t = time.perf_counter()
        r = post(f"{OLLAMA}/api/chat", cuerpo)
    total = time.perf_counter() - t
    gen = r.get("eval_duration", 0) / 1e9
    return {"texto": r["message"]["content"], "s": total, "carga_s": r.get("load_duration", 0) / 1e9,
            "tok_salida": r.get("eval_count", 0), "tok_s": r.get("eval_count", 0) / gen if gen else 0}


def llamar_openrouter(modelo: str, sistema: str, texto: str) -> dict:
    clave = os.environ.get("OPENROUTER_API_KEY")
    if not clave:
        sys.exit("Falta OPENROUTER_API_KEY")
    cuerpo = {"model": modelo, "temperature": 0, "max_tokens": tope_salida(texto), "reasoning": {"enabled": False},
              "messages": [{"role": "system", "content": sistema}, {"role": "user", "content": texto}]}
    cab = {"Authorization": f"Bearer {clave}", "X-Title": "iurevoz-bench"}
    t = time.perf_counter()
    try:
        r = post(OPENROUTER, cuerpo, cab)
    except urllib.error.HTTPError as e:
        if e.code != 400:
            raise
        cuerpo.pop("reasoning")
        t = time.perf_counter()
        r = post(OPENROUTER, cuerpo, cab)
    total = time.perf_counter() - t
    salida = r.get("usage", {}).get("completion_tokens", 0)
    return {"texto": r["choices"][0]["message"]["content"] or "", "s": total, "carga_s": 0.0,
            "tok_salida": salida, "tok_s": salida / total if total else 0,
            "proveedor": r.get("provider", "")}


def backend(spec: str):
    tipo, _, modelo = spec.partition(":")
    if tipo == "ollama":
        return modelo, llamar_ollama
    if tipo == "openrouter":
        return modelo, llamar_openrouter
    sys.exit(f"Modelo desconocido: {spec} (usa ollama:<modelo> u openrouter:<modelo>)")


def limpiar(texto: str) -> str:
    texto = re.sub(r"<think>.*?</think>", "", texto, flags=re.S).strip()
    return texto.strip("\"“”«» \n")


# ── principal ─────────────────────────────────────────────────────────────────

def evaluar(spec: str, casos: list[dict], sistema: str, mostrar: bool) -> dict:
    modelo, llamar = backend(spec)
    print(f"→ {spec} …", flush=True)
    calentamiento = llamar(modelo, sistema, "hola esto es una prueba")
    filas = []
    for c in casos:
        try:
            r = llamar(modelo, sistema, c["entrada"])
            salida = limpiar(r["texto"])
        except (urllib.error.URLError, TimeoutError, KeyError) as e:
            # Un fallo deja el texto sin corregir y se cuenta aparte.
            r = {"s": float("nan"), "tok_salida": 0, "tok_s": 0}
            salida = c["entrada"]
            print(f"  [{c['id']}] falló: {e}", flush=True)
        ref_p, ent_p, sal_p = palabras(c["ref"]), palabras(c["entrada"]), palabras(salida)
        antes, despues = aciertos(ref_p, ent_p), aciertos(ref_p, sal_p)
        fila = {
            "id": c["id"], "salida": salida, "s": r["s"], "tok_salida": r["tok_salida"], "tok_s": r["tok_s"],
            "wer_antes": tasa_error(ref_p, ent_p), "wer_despues": tasa_error(ref_p, sal_p),
            "formato_antes": tasa_error(fichas(c["ref"]), fichas(c["entrada"])),
            "formato_despues": tasa_error(fichas(c["ref"]), fichas(salida)),
            "arregladas": len(despues - antes), "rotas": len(antes - despues),
            "cambios": cambios(ent_p, sal_p), "palabras": len(ent_p), "fallo": r["s"] != r["s"],
        }
        filas.append(fila)
        if mostrar:
            print(f"\n  [{c['id']}] {r['s']:.1f}s  arregladas {fila['arregladas']}  rotas {fila['rotas']}")
            print(f"    entrada  {c['entrada']}")
            print(f"    salida   {salida}")
            if fila["cambios"]:
                print("    cambios  " + " · ".join(f"{a or '∅'} → {b or '∅'}" for a, b in fila["cambios"]))

    def media(k):
        return statistics.fmean(f[k] for f in filas)

    ok = [f for f in filas if not f["fallo"]]
    seg = sorted(f["s"] for f in ok) or [float("nan")]
    cortas = [f["s"] for f in ok if "/frase-" in f["id"]]
    return {
        "modelo": spec, "carga_s": calentamiento["carga_s"], "filas": filas,
        "s_p50": statistics.median(seg), "s_max": seg[-1],
        "s_frase": statistics.median(cortas) if cortas else float("nan"), "fallos": len(filas) - len(ok),
        "tok_s": statistics.fmean(f["tok_s"] for f in filas if f["tok_s"]) if any(f["tok_s"] for f in filas) else 0,
        "wer_antes": media("wer_antes"), "wer_despues": media("wer_despues"),
        "formato_antes": media("formato_antes"), "formato_despues": media("formato_despues"),
        "arregladas": sum(f["arregladas"] for f in filas), "rotas": sum(f["rotas"] for f in filas),
        "cambios": sum(len(f["cambios"]) for f in filas),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("modelos", nargs="+", help="ollama:<modelo> u openrouter:<modelo>")
    ap.add_argument("--resultados", type=Path, default=AQUI / "resultados.json",
                    help="salida --json de stt_bench.py (por omisión resultados.json)")
    ap.add_argument("--glosario", type=Path, help="términos del dominio, uno por línea")
    ap.add_argument("--solo", help="filtra casos cuyo id contenga alguno de estos textos (coma)")
    ap.add_argument("--mostrar", action="store_true", help="muestra entrada, salida y cambios por caso")
    ap.add_argument("--json", type=Path, help="guarda los resultados completos")
    a = ap.parse_args()

    casos = cargar_casos(a.resultados)
    if a.solo:
        claves = a.solo.split(",")
        casos = [c for c in casos if any(k in c["id"] for k in claves)]
    if not casos:
        sys.exit(f"No hay casos: {a.resultados} necesita audios con su .txt de referencia")
    sistema = SISTEMA
    if a.glosario:
        terminos = [t.strip() for t in a.glosario.read_text(encoding="utf-8").splitlines() if t.strip()]
        sistema += " Términos que pueden aparecer: " + ", ".join(terminos) + "."
    print(f"{len(casos)} casos ({sum(len(palabras(c['entrada'])) for c in casos)} palabras)\n")

    res = [evaluar(m, casos, sistema, a.mostrar) for m in a.modelos]

    print(f"\n{'modelo':28} {'carga s':>7} {'frase s':>7} {'p50 s':>6} {'máx s':>6} {'tok/s':>6} "
          f"{'WER antes→después':>18} {'formato antes→después':>22} {'arregl.':>7} {'rotas':>5} {'cambios':>7} {'fallos':>6}")
    print("─" * 135)
    for r in res:
        print(f"{r['modelo']:28} {r['carga_s']:7.1f} {r['s_frase']:7.2f} {r['s_p50']:6.2f} {r['s_max']:6.2f} "
              f"{r['tok_s']:6.1f} {r['wer_antes']:8.1%} → {r['wer_despues']:6.1%} "
              f"{r['formato_antes']:11.1%} → {r['formato_despues']:6.1%} "
              f"{r['arregladas']:7d} {r['rotas']:5d} {r['cambios']:7d} {r['fallos']:6d}")
    print("\nfrase s = mediana en frases sueltas (~15 palabras); p50/máx sobre todos los casos.")
    print("WER sin puntuación ni mayúsculas; formato cuenta también signos y mayúsculas.")
    print("arregladas = palabras mal oídas que quedaron bien; rotas = palabras correctas que el LLM cambió.")
    print("cambios = tramos de palabras que tocó el LLM (lo que la salvaguarda resaltaría).")
    print("fallos = casos con error o más de 120 s; cuentan como texto sin corregir.")

    if a.json:
        a.json.write_text(json.dumps({"sistema": sistema, "casos": casos, "resultados": res},
                                     ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"\nResultados en {a.json}")


if __name__ == "__main__":
    main()
