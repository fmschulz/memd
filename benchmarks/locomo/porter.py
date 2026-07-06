"""Classic Porter stemmer (Porter, 1980), self-contained.

Used to mirror the upstream LoCoMo F1 metric, whose tokens are Porter-stemmed
(task_eval/evaluation.py in snap-research/locomo uses nltk's PorterStemmer).
This is the original algorithm; nltk's default mode adds small extensions, so
absolute F1 may differ marginally from other harnesses. All comparisons in
this harness use this same stemmer, which is what matters.
"""

VOWELS = "aeiou"


def _is_consonant(word, i):
    ch = word[i]
    if ch in VOWELS:
        return False
    if ch == "y":
        return i == 0 or not _is_consonant(word, i - 1)
    return True


def _measure(stem):
    """Number of VC sequences in the stem."""
    forms = []
    for i in range(len(stem)):
        forms.append("c" if _is_consonant(stem, i) else "v")
    m = 0
    prev = None
    for f in forms:
        if prev == "v" and f == "c":
            m += 1
        prev = f
    return m


def _contains_vowel(stem):
    return any(not _is_consonant(stem, i) for i in range(len(stem)))


def _ends_double_consonant(word):
    return (
        len(word) >= 2
        and word[-1] == word[-2]
        and _is_consonant(word, len(word) - 1)
    )


def _ends_cvc(word):
    if len(word) < 3:
        return False
    if not _is_consonant(word, len(word) - 3):
        return False
    if _is_consonant(word, len(word) - 2):
        return False
    if not _is_consonant(word, len(word) - 1):
        return False
    return word[-1] not in "wxy"


def _replace_suffix(word, suffix, replacement, m_min):
    stem = word[: len(word) - len(suffix)]
    if _measure(stem) > m_min:
        return stem + replacement
    return word


def stem(word):
    word = word.lower()
    if len(word) <= 2:
        return word

    # Step 1a
    if word.endswith("sses"):
        word = word[:-2]
    elif word.endswith("ies"):
        word = word[:-2]
    elif word.endswith("ss"):
        pass
    elif word.endswith("s"):
        word = word[:-1]

    # Step 1b
    flag_1b = False
    if word.endswith("eed"):
        stem_ = word[:-3]
        if _measure(stem_) > 0:
            word = word[:-1]
    elif word.endswith("ed"):
        stem_ = word[:-2]
        if _contains_vowel(stem_):
            word = stem_
            flag_1b = True
    elif word.endswith("ing"):
        stem_ = word[:-3]
        if _contains_vowel(stem_):
            word = stem_
            flag_1b = True
    if flag_1b:
        if word.endswith(("at", "bl", "iz")):
            word += "e"
        elif _ends_double_consonant(word) and not word.endswith(("l", "s", "z")):
            word = word[:-1]
        elif _measure(word) == 1 and _ends_cvc(word):
            word += "e"

    # Step 1c
    if word.endswith("y") and _contains_vowel(word[:-1]):
        word = word[:-1] + "i"

    # Step 2
    step2 = [
        ("ational", "ate"), ("tional", "tion"), ("enci", "ence"), ("anci", "ance"),
        ("izer", "ize"), ("abli", "able"), ("alli", "al"), ("entli", "ent"),
        ("eli", "e"), ("ousli", "ous"), ("ization", "ize"), ("ation", "ate"),
        ("ator", "ate"), ("alism", "al"), ("iveness", "ive"), ("fulness", "ful"),
        ("ousness", "ous"), ("aliti", "al"), ("iviti", "ive"), ("biliti", "ble"),
    ]
    for suffix, repl in step2:
        if word.endswith(suffix):
            word = _replace_suffix(word, suffix, repl, 0)
            break

    # Step 3
    step3 = [
        ("icate", "ic"), ("ative", ""), ("alize", "al"), ("iciti", "ic"),
        ("ical", "ic"), ("ful", ""), ("ness", ""),
    ]
    for suffix, repl in step3:
        if word.endswith(suffix):
            word = _replace_suffix(word, suffix, repl, 0)
            break

    # Step 4
    step4 = [
        "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement",
        "ment", "ent", "ou", "ism", "ate", "iti", "ous", "ive", "ize",
    ]
    for suffix in step4:
        if word.endswith(suffix):
            stem_ = word[: len(word) - len(suffix)]
            if _measure(stem_) > 1:
                word = stem_
            break
        if suffix == "ent" and word.endswith("ion"):
            pass
    # special "ion" case: only strip if preceded by s or t
    if word.endswith("ion"):
        stem_ = word[:-3]
        if _measure(stem_) > 1 and stem_.endswith(("s", "t")):
            word = stem_

    # Step 5a
    if word.endswith("e"):
        stem_ = word[:-1]
        m = _measure(stem_)
        if m > 1 or (m == 1 and not _ends_cvc(stem_)):
            word = stem_

    # Step 5b
    if _measure(word) > 1 and _ends_double_consonant(word) and word.endswith("l"):
        word = word[:-1]

    return word


if __name__ == "__main__":
    # Known Porter outputs (from the published algorithm's test vocabulary).
    cases = {
        "caresses": "caress", "ponies": "poni", "ties": "ti", "caress": "caress",
        "cats": "cat", "feed": "feed", "agreed": "agre", "plastered": "plaster",
        "bled": "bled", "motoring": "motor", "sing": "sing", "conflated": "conflat",
        "troubled": "troubl", "sized": "size", "hopping": "hop", "tanned": "tan",
        "falling": "fall", "hissing": "hiss", "fizzed": "fizz", "failing": "fail",
        "filing": "file", "happy": "happi", "sky": "sky", "relational": "relat",
        "conditional": "condit", "rational": "ration", "valenci": "valenc",
        "digitizer": "digit", "operator": "oper", "feudalism": "feudal",
        "decisiveness": "decis", "hopefulness": "hope", "formaliti": "formal",
        "triplicate": "triplic", "formative": "form", "formalize": "formal",
        "electriciti": "electr", "electrical": "electr", "hopeful": "hope",
        "goodness": "good", "revival": "reviv", "allowance": "allow",
        "inference": "infer", "airliner": "airlin", "adjustable": "adjust",
        "defensible": "defens", "irritant": "irrit", "replacement": "replac",
        "adjustment": "adjust", "dependent": "depend", "adoption": "adopt",
        "communism": "commun", "activate": "activ", "angulariti": "angular",
        "homologous": "homolog", "effective": "effect", "bowdlerize": "bowdler",
        "probate": "probat", "rate": "rate", "cease": "ceas",
        "controll": "control", "roll": "roll",
    }
    bad = {w: (stem(w), want) for w, want in cases.items() if stem(w) != want}
    if bad:
        raise SystemExit(f"porter self-test FAILED: {bad}")
    print(f"porter self-test OK ({len(cases)} cases)")
