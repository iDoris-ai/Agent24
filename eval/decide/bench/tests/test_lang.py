from decide_bench.lang import infer_lang, resolve_lang
from decide_bench.types import EvalItem


def test_infer_lang_zh():
    assert infer_lang("记住我对花生过敏") == "zh"


def test_infer_lang_en():
    assert infer_lang("remember that I am allergic to peanuts") == "en"


def test_infer_lang_th():
    assert infer_lang("จำไว้ว่าฉันแพ้ถั่วลิสง") == "th"


def test_infer_lang_empty_defaults_en():
    assert infer_lang("") == "en"


def test_infer_lang_mixed_cjk_wins_over_latin():
    # a Chinese sentence quoting a Latin proper noun is still zh under
    # this coarse rule — documented as an imperfection in lang.py.
    assert infer_lang("记住我喜欢用 Python 写代码") == "zh"


def test_resolve_lang_prefers_explicit_field():
    item = EvalItem(
        id="x1", point="retain_intent", input="remember X", expected="remember", cost_level="low", lang="th"
    )
    lang, inferred = resolve_lang(item)
    assert lang == "th"
    assert inferred is False


def test_resolve_lang_falls_back_to_inference():
    item = EvalItem(id="x2", point="retain_intent", input="记住我喜欢猫", expected="remember", cost_level="low")
    lang, inferred = resolve_lang(item)
    assert lang == "zh"
    assert inferred is True


def test_resolve_lang_tool_risk_renders_input_and_context():
    item = EvalItem(
        id="x3",
        point="tool_risk",
        input={"tool": "delete_file", "args": "secret.env"},
        expected="high",
        cost_level="high",
        context="泰文 ไฟล์ลับ",
    )
    lang, inferred = resolve_lang(item)
    assert lang == "th"  # Thai characters in context win
    assert inferred is True
