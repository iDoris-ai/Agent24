from decide_bench.overlap import DEFAULT_THRESHOLD, char_ngrams, check_overlap, jaccard, seq_ratio


def test_char_ngrams_basic():
    assert char_ngrams("abcd", n=3) == {"abc", "bcd"}


def test_char_ngrams_short_string_falls_back_to_whole_string():
    assert char_ngrams("ab", n=3) == {"ab"}
    assert char_ngrams("", n=3) == set()


def test_jaccard_identical_and_disjoint():
    assert jaccard({"a", "b"}, {"a", "b"}) == 1.0
    assert jaccard({"a"}, {"b"}) == 0.0
    assert jaccard(set(), set()) == 1.0
    assert jaccard({"a"}, set()) == 0.0


def test_seq_ratio_matches_known_pair_from_pr718_review():
    # The exact pair clestons' PR #718 review flagged at similarity 0.94
    # (difflib.SequenceMatcher), used here as a known-good calibration
    # point for our implementation of the same metric.
    a = '工具调用：web_search，参数：query="北京 明天 天气"'
    b = '工具调用：web_search，参数：query="上海 明天 天气"'
    assert seq_ratio(a, b) > 0.9


def test_check_overlap_detects_known_near_duplicate_pair():
    # Synthetic sanity check independent of the real train/eval files:
    # a near-duplicate template swap must be flagged at the default 0.7
    # threshold by at least one of the two metrics.
    from decide_bench.overlap import OverlapHit

    hit = OverlapHit(
        point="tool_risk",
        eval_id="synthetic-1",
        eval_text='工具调用：web_search，参数：query="北京 明天 天气"',
        train_label="low",
        train_text='工具调用：web_search，参数：query="上海 明天 天气"',
        ngram_jaccard=jaccard(
            char_ngrams('工具调用：web_search，参数：query="北京 明天 天气"'),
            char_ngrams('工具调用：web_search，参数：query="上海 明天 天气"'),
        ),
        seq_ratio=seq_ratio(
            '工具调用：web_search，参数：query="北京 明天 天气"',
            '工具调用：web_search，参数：query="上海 明天 天气"',
        ),
    )
    assert hit.similarity >= DEFAULT_THRESHOLD


def test_no_overlap_between_published_train_and_eval_sets():
    """D0-6 acceptance: after rewriting train_data/*.jsonl to drop the
    near-duplicate templates PR #718's review found (clestons,
    2026-10-07), this must be 0 at the default threshold for every
    decision point. Regression guard against someone reintroducing a
    template-swap duplicate later.
    """
    hits = check_overlap()
    assert hits == [], f"train/eval overlap reappeared: {hits!r}"
