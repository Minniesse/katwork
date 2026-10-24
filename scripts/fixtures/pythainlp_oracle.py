#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Write katwork-thai's oracle fixtures with PyThaiNLP.

The oracle is PyThaiNLP 5.4.0 (https://github.com/PyThaiNLP/pythainlp),
licensed Apache-2.0. It runs for developers and CI only; nothing here ships.

Needs Python 3.11 or newer. From the repository root, install the pinned
wheel (pip checks its SHA-256 and runs no setup code), then run offline:

    pip install --require-hashes --only-binary=:all: -r scripts/fixtures/requirements.txt
    PYTHAINLP_OFFLINE=1 python3 scripts/fixtures/pythainlp_oracle.py

It writes two files and nothing else:
  crates/katwork-thai/tests/fixtures/normalize.json  pythainlp.util.normalize
  crates/katwork-thai/tests/fixtures/numbers.json    thaiword_to_num and words_to_num

Every input is synthetic and built below. In numbers.json, `expected` is a
decimal string computed from the integers thaiword_to_num returns, never from
the float words_to_num returns (that float is only a cross-check), and null
when the oracle raises. Marks, zero-width and space-like characters are
written as backslash-u escapes, because editors reorder Thai marks.

Every string in this file that is not ASCII is written with escapes for the
same reason.
"""

import decimal
import itertools
import os
import pathlib
import sys

if sys.version_info < (3, 11):
    sys.exit("pythainlp_oracle.py needs Python 3.11 or newer")
os.environ["PYTHAINLP_OFFLINE"] = "1"

import pythainlp  # noqa: E402
from pythainlp.util import normalize, thaiword_to_num, words_to_num  # noqa: E402

if pythainlp.__version__ != "5.4.0":
    sys.exit(f"expected PyThaiNLP 5.4.0, found {pythainlp.__version__}")

OUT = pathlib.Path(__file__).resolve().parents[2] / "crates/katwork-thai/tests/fixtures"

# Consonants and the marks the cases are built from.
KO, KHO, NO, THO, DO, PHO, MO, SO, HO, PHO_SAMPHAO, RU, LU = (
    "\u0e01", "\u0e02", "\u0e19", "\u0e17", "\u0e14", "\u0e1e", "\u0e21", "\u0e2a",
    "\u0e2b", "\u0e20", "\u0e24", "\u0e26",
)
TONES = ["\u0e48", "\u0e49", "\u0e4a", "\u0e4b"]
THANTHAKHAT = "\u0e4c"
ABOVE = ["\u0e31", "\u0e34", "\u0e35", "\u0e36", "\u0e37", "\u0e4d", "\u0e47"]
BELOW = ["\u0e38", "\u0e39"]
FOLLOW = ["\u0e30", "\u0e32", "\u0e33", "\u0e45"]
LEAD = ["\u0e40", "\u0e41", "\u0e42", "\u0e43", "\u0e44"]
SIGNS = ["\u0e3a", THANTHAKHAT, "\u0e4d", "\u0e4e"]
NIKHAHIT, SARA_AA, SARA_AM, SARA_E, LAKKHANGYAO = "\u0e4d", "\u0e32", "\u0e33", "\u0e40", "\u0e45"


def normalize_inputs():
    cases = []
    add = cases.append
    # SARA AM written as one code point, as nikhahit and aa, and with tone marks on either side.
    for base in [NO, KHO, THO, DO]:
        add(base + SARA_AM)
        add(base + NIKHAHIT + SARA_AA)
        for tone in TONES:
            add(base + tone + SARA_AM)
            add(base + SARA_AM + tone)
            add(base + tone + NIKHAHIT + SARA_AA)
            add(base + NIKHAHIT + tone + SARA_AA)
    add(NO + NIKHAHIT + TONES[0] + TONES[1] + SARA_AA)
    add(NO + NIKHAHIT + LAKKHANGYAO)
    add(NO + SARA_AM + SARA_AM)
    # A tone mark or thanthakhat before and after each above and below vowel.
    for mark in TONES + [THANTHAKHAT]:
        for vowel in ABOVE + BELOW:
            add(KO + mark + vowel)
            add(KO + vowel + mark)
    add(THO + TONES[0] + "\u0e35" + "\u0e38")
    add(KO + TONES[0] + TONES[1] + "\u0e34")
    add(KO + "\u0e3a" + TONES[0])
    # Runs of sara e, before and after a consonant.
    for count in range(1, 6):
        add(SARA_E * count + KO)
        add(KO + SARA_E * count)
    add(SARA_E + SARA_E + "\u0e14\u0e07")
    # Repeated vowels and signs, adjacent and with spaces between.
    for mark in FOLLOW + ABOVE + BELOW + SIGNS + LEAD:
        add(DO + mark + mark)
        add(DO + mark + mark + mark)
        add(DO + mark + " " + mark)
    add(DO + "\u0e35 \u0e35\u0e35")
    add(DO + "\u0e35  \u0e35")
    add(MO + SARA_AA + MO + SARA_AA + SARA_AA + SARA_AA)
    # Two or three tone marks in a row.
    for first, second in itertools.product(TONES, TONES):
        add(KO + first + second)
    add(KO + TONES[0] + TONES[1] + TONES[2])
    add(KO + "\u0e35" + TONES[0] + TONES[3])
    # Lakkhangyao after a consonant, repeated, after ru and lu, and alone.
    for word in [
        KO + LAKKHANGYAO,
        KO + LAKKHANGYAO * 2,
        KO + LAKKHANGYAO * 3,
        KO + LAKKHANGYAO * 4,
        RU + LAKKHANGYAO,
        LU + LAKKHANGYAO,
        RU + LAKKHANGYAO * 2,
        LAKKHANGYAO,
        LAKKHANGYAO + KO,
        " " + LAKKHANGYAO,
    ]:
        add(word)
    # Dangling marks at the start and after a space; a space before a mark.
    for mark in TONES + ABOVE[:3] + BELOW + SIGNS:
        add(mark + KHO)
        add(KO + " " + mark)
        add(SARA_E + KO + " " + mark)
        add(KO + SARA_AA + " " + mark)
        add(KO + " " + mark + KHO)
    add(TONES[0] + "\u0e31" + KO)
    add(" " + TONES[0] + KO)
    add(PHO_SAMPHAO + SARA_AA + PHO + " \u0e38" + TONES[0])
    add(PHO + " \u0e38" + TONES[0] + "\u0e21")
    add(KO + " " + TONES[0] + " " + TONES[1])
    add(KO + SARA_AM + " " + TONES[0] + THO + "\u0e35" + TONES[0])
    add("\u0e44\u0e21 " + TONES[0])
    add(KO + "  " + TONES[0])
    # Zero-width and other invisible characters inside and around words.
    for invisible in ["\u200b", "\u200c", "\u200d", "\u2060", "\ufeff", "\u00ad"]:
        add(KO + invisible + KHO)
        add(invisible + NO + TONES[1] + SARA_AM + invisible)
        add(KO + invisible + TONES[0])
    add("\u0e23\u0e32\u0e22\u0e44\u0e14" + TONES[1] + "\u200b\u0e23\u0e27\u0e21")
    add("\u200b\u200c")
    # Spaces, newlines and other white space.
    for text in [
        KO + "  " + KHO,
        KO + "     " + KHO,
        " " + KO + " ",
        "  " + KO + "  " + KHO + "  ",
        KO + " \n\n " + KHO,
        KO + "\n \n" + KHO,
        KO + "\n\n\n" + KHO,
        "\n\n" + KO + "\n\n",
        KO + "\r\n" + KHO,
        KO + "\t\t" + KHO,
        KO + "\t \t" + KHO,
        "\u3000" + KO + "\u3000",
        "\u3000\u3000" + KO + " " + KHO + "\u3000",
        "\u001c" + KO + "\u001c",
        "\u001f" + KO + "\u001d",
        KO + "\u00a0" + KHO,
        KO + "\u00a0\u00a0" + KHO,
        "\u00a0" + KO + "\u00a0",
        KO + " \u00a0 " + KHO,
        KO + "\u2003" + KHO,
        "\u2028" + KO + "\u2029",
        "\u0085" + KO,
        KO + "\u000b" + KHO + "\u000c",
        "",
        " ",
        "\n",
        TONES[0] + TONES[1],
    ]:
        add(text)
    # Mixed Thai, Latin and digits.
    for text in [
        "Total \u0e23\u0e32\u0e22\u0e44\u0e14" + TONES[1] + "  \u0e51,\u0e52\u0e55\u0e50  \u0e25" + TONES[1] + "\u0e32\u0e19\u0e1a\u0e32\u0e17",
        "FY2569  budget  3,450",
        "\ufb01nancial \u0e1b\u0e35 2567",
        "didn\u2019t \u0e44\u0e21" + TONES[0],
        "m\u00b2 10\u00b2",
        "\u0130stanbul",
        "ABC " + KO + TONES[0] + " DEF",
        "\uff11\uff12\uff13 " + KO,
        "\u2212" + "1,250 \u2013 10",
    ]:
        add(text)
    # Words written in several orders.
    for text in [
        THO + "\u0e35" + TONES[0],
        THO + TONES[0] + "\u0e35",
        "\u0e44\u0e21" + TONES[0],
        "\u0e44\u0e21" + TONES[0] + TONES[0],
        KO + "\u0e47",
        KO + " \u0e47",
        "\u0e40\u0e17" + TONES[0] + "\u0e32\u0e19\u0e31" + TONES[1] + "\u0e19",
        "\u0e40\u0e17\u0e32" + TONES[0] + "\u0e19" + TONES[1] + "\u0e31\u0e19",
        THO + "\u0e31" + TONES[1] + "\u0e07\u0e2b\u0e21\u0e14",
        THO + TONES[1] + "\u0e31\u0e07\u0e2b\u0e21\u0e14",
        "\u0e01\u0e23\u0e30\u0e17\u0e23\u0e27\u0e07" + " " + "\u0e01\u0e32\u0e23" + "\u0e04\u0e25\u0e31\u0e07",
        SO + "\u0e30\u0e30",
        SO + "\u0e30 \u0e30",
        MO + "\u0e35" + MO + "\u0e35",
        MO + "\u0e35\u0e35" + MO + "\u0e35\u0e35",
    ]:
        add(text)
    return cases


DIGIT_WORDS = [
    "\u0e28\u0e39\u0e19\u0e22\u0e4c",  # 0
    "\u0e2b\u0e19\u0e36\u0e48\u0e07",  # 1
    "\u0e2a\u0e2d\u0e07",  # 2
    "\u0e2a\u0e32\u0e21",  # 3
    "\u0e2a\u0e35\u0e48",  # 4
    "\u0e2b\u0e49\u0e32",  # 5
    "\u0e2b\u0e01",  # 6
    "\u0e40\u0e08\u0e47\u0e14",  # 7
    "\u0e41\u0e1b\u0e14",  # 8
    "\u0e40\u0e01\u0e49\u0e32",  # 9
]
ET = "\u0e40\u0e2d\u0e47\u0e14"
YI = "\u0e22\u0e35\u0e48"
PLACES = ["", "\u0e2a\u0e34\u0e1a", "\u0e23\u0e49\u0e2d\u0e22", "\u0e1e\u0e31\u0e19", "\u0e2b\u0e21\u0e37\u0e48\u0e19", "\u0e41\u0e2a\u0e19"]
SIP, ROI, PHAN, MUEN, SAEN = PLACES[1:]
LAN = "\u0e25\u0e49\u0e32\u0e19"
LOP = "\u0e25\u0e1a"
CHUT = "\u0e08\u0e38\u0e14"
SUN = DIGIT_WORDS[0]
WORD_LIST = DIGIT_WORDS[1:] + [ET, YI] + PLACES[1:] + [LAN, LOP, CHUT, SUN]


def six_figures(n):
    """Thai words for 0 < n < 1,000,000, as people write them."""
    digits = str(n)
    words = ""
    for index, char in enumerate(digits):
        digit, place = int(char), len(digits) - 1 - index
        if digit == 0:
            continue
        if place == 1 and digit == 1:
            words += SIP
        elif place == 1 and digit == 2:
            words += YI + SIP
        elif place == 0 and digit == 1 and len(digits) > 1:
            words += ET
        else:
            words += DIGIT_WORDS[digit] + PLACES[place]
    return words


def to_words(n):
    """Thai number words for an integer, with LAN between groups of six digits."""
    if n == 0:
        return SUN
    if n < 0:
        return LOP + to_words(-n)
    groups = []
    while n:
        groups.append(n % 1_000_000)
        n //= 1_000_000
    groups.reverse()
    return LAN.join(six_figures(group) if group else "" for group in groups)


def split_words(text):
    """Split text by longest match over the numeral word list; other runs stay one word."""
    words, rest = [], ""
    index = 0
    while index < len(text):
        match = max((w for w in WORD_LIST if text.startswith(w, index)), key=len, default=None)
        if match is None:
            rest += text[index]
            index += 1
            continue
        if rest:
            words.append(rest)
            rest = ""
        words.append(match)
        index += len(match)
    if rest:
        words.append(rest)
    return words


def number_inputs():
    cases = []
    add = cases.append
    values = list(range(0, 121)) + [
        150, 199, 200, 201, 211, 221, 250, 999, 1000, 1001, 1010, 1011, 1100, 1234, 2000, 2021,
        9999, 10000, 10001, 12345, 20000, 21021, 99999, 100000, 100001, 123456, 999999,
        1_000_000, 1_000_001, 1_000_011, 1_250_000, 2_300_612, 10_000_000, 12_345_678, 100_000_000,
        1_000_000_000, 1_250_000_000, 10_000_000_000, 100_000_000_000, 1_000_000_000_000,
        3_480_000_000_000, 1_000_000_000_001, 999_999_999_999, 1_234_567_890_123,
        10**18, 10**18 + 5, 10**24, 79 * 10**27,
    ]
    for value in values:
        add(to_words(value))
    for value in [1, 15, 100, 1250, 1_000_000, 10**12]:
        add(to_words(-value))
    # Decimals: the integer part in words, then one digit word per decimal digit.
    for integer, decimals in [
        (0, "5"), (0, "05"), (0, "125"), (1, "25"), (3, "48"), (12, "5"), (50, "95"),
        (99, "99"), (1250, "75"), (0, "0"), (7, "000"), (1, "1234567890"),
        (0, "1" * 20), (-1, "5"), (-12, "25"),
    ]:
        add(to_words(integer) + CHUT + "".join(DIGIT_WORDS[int(d)] for d in decimals))
    # Short forms people write: a bare power word, a bare million, and LAN repeated.
    for text in [
        SIP, ROI, PHAN, MUEN, SAEN, LAN, LAN * 2, LAN * 3, LAN * 4, SIP + LAN, ROI + LAN,
        PHAN + LAN, MUEN + LAN, SAEN + LAN, PHAN + LAN + LAN, DIGIT_WORDS[3] + LAN + DIGIT_WORDS[5] + SAEN,
        ROI + ET, SIP + ET, YI + SIP + ET, SAEN + LAN + SAEN, DIGIT_WORDS[1] + LAN + LAN,
    ]:
        add(text)
    # Forms the oracle accepts although nobody writes them; the Rust port follows it.
    for text in [
        LOP, ET, YI, ET + SIP, YI + ROI, DIGIT_WORDS[1] + SIP, LOP + CHUT + DIGIT_WORDS[5],
        DIGIT_WORDS[2] + CHUT + SIP, DIGIT_WORDS[1] + CHUT + LOP, DIGIT_WORDS[1] + CHUT,
        SUN + CHUT + SUN, DIGIT_WORDS[1] + CHUT + ROI, LOP + LAN,
    ]:
        add(text)
    # Inputs the oracle rejects.
    for text in [
        "", SUN + DIGIT_WORDS[2], DIGIT_WORDS[2] + DIGIT_WORDS[3], SIP + ROI, MUEN + SAEN,
        ROI + ROI, PHAN + PHAN, LOP + LOP, DIGIT_WORDS[1] + ROI + LOP, LOP + SUN, CHUT,
        CHUT + DIGIT_WORDS[5], DIGIT_WORDS[1] + CHUT + CHUT, DIGIT_WORDS[1] + CHUT + DIGIT_WORDS[2] + CHUT + DIGIT_WORDS[3],
        DIGIT_WORDS[3] + "\u0e32\u0e23\u0e16",  # "can": the word for three, then more letters
        "\u0e2d\u0e38\u0e15\u0e2a\u0e32\u0e2b\u0e01\u0e23\u0e23\u0e21",  # "industry": six inside a word
        DIGIT_WORDS[9] + "\u0e2d\u0e35\u0e49",  # "chair": the word for nine, then more letters
        DIGIT_WORDS[2] + " " + ROI, "100", ROI + "x", SUN + SUN,
    ]:
        add(text)
    return cases


def number_case(text):
    """The oracle's value for text as an exact decimal string, or None when it raises."""
    words = split_words(text)
    try:
        if CHUT not in words:
            exact = decimal.Decimal(thaiword_to_num(text))
            check = float(exact)
        else:
            point = words.index(CHUT)
            integer = thaiword_to_num("".join(words[:point]))
            fraction = sum(
                (decimal.Decimal(thaiword_to_num(word)).scaleb(-(place + 1)) for place, word in enumerate(words[point + 1 :])),
                decimal.Decimal(0),
            )
            exact = decimal.Decimal(integer) - fraction if integer <= -1 else decimal.Decimal(integer) + fraction
            check = words_to_num(words)
    except ValueError:
        return None
    # The float result must agree; it only checks that the exact sum follows words_to_num.
    if abs(float(exact) - float(check)) > 1e-9 * max(1.0, abs(float(exact))):
        sys.exit(f"exact and float results differ for {text!r}: {exact} {check}")
    return format(exact, "f")


def escape(text):
    """A JSON string with every non-ASCII character except Thai consonants and digits escaped."""
    out = []
    for char in text:
        code = ord(char)
        if char in '"\\':
            out.append("\\" + char)
        elif 0x20 <= code < 0x7F or 0x0E01 <= code <= 0x0E30 or 0x0E4F <= code <= 0x0E5B:
            out.append(char)
        elif code > 0xFFFF:
            code -= 0x10000
            out.append(f"\\u{0xD800 + (code >> 10):04x}\\u{0xDC00 + (code & 0x3FF):04x}")
        else:
            out.append(f"\\u{code:04x}")
    return '"' + "".join(out) + '"'


def write(name, oracle, cases):
    lines = [f'{{"oracle": {escape(oracle)}, "cases": [']
    for index, (text, expected) in enumerate(cases):
        value = "null" if expected is None else escape(expected)
        comma = "," if index + 1 < len(cases) else ""
        lines.append(f'  {{"input": {escape(text)}, "expected": {value}}}{comma}')
    lines.append("]}")
    (OUT / name).write_text("\n".join(lines) + "\n", encoding="utf-8")


def unique(items):
    seen = set()
    return [item for item in items if not (item in seen or seen.add(item))]


def main():
    decimal.getcontext().prec = 120
    for value in itertools.chain(range(-100, 20_000), (10**12 + 7, 3 * 10**18 + 21)):
        if thaiword_to_num(to_words(value)) != value:
            sys.exit(f"to_words is wrong for {value}")
    normalize_cases = [(text, normalize(text)) for text in unique(normalize_inputs())]
    number_cases = [(text, number_case(text)) for text in unique(number_inputs())]
    write("normalize.json", "pythainlp 5.4.0 pythainlp.util.normalize", normalize_cases)
    write("numbers.json", "pythainlp 5.4.0 thaiword_to_num, words_to_num", number_cases)
    print(f"normalize.json: {len(normalize_cases)} cases; numbers.json: {len(number_cases)} cases")


if __name__ == "__main__":
    main()
