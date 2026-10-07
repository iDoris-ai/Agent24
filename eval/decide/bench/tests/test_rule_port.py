from decide_bench.adapters.rule import RETAIN_RS_TEST_TABLE, explicit_remember, verify_port


def test_matches_retain_rs_table_exactly():
    verify_port()


def test_table_has_expected_size():
    # retain.rs's `recognizes_only_the_supported_leading_forms` has 30 cases.
    assert len(RETAIN_RS_TEST_TABLE) == 30


def test_individual_cases_for_readable_failures():
    for prompt, expected in RETAIN_RS_TEST_TABLE:
        assert explicit_remember(prompt) == expected, prompt
