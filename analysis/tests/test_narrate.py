import json
from types import SimpleNamespace

from chronoforge_ai import grounded, narrate

EVIDENCE = {
    "outcome": {"kind": "assertion failed", "step": 99, "line": 28},
    "hypotheses": [{"title": "Lost update", "evidence": [{"step": 17}, {"step": 27}, {"step": 47}]}],
}


class FakeClient:
    """Stands in for anthropic.Anthropic so tests never hit the network."""

    def __init__(self, payload, stop_reason="end_turn"):
        self.payload = payload
        self.stop_reason = stop_reason
        self.kwargs = None
        self.beta = SimpleNamespace(messages=SimpleNamespace(create=self.create))

    def create(self, **kwargs):
        self.kwargs = kwargs
        block = SimpleNamespace(type="text", text=json.dumps(self.payload))
        return SimpleNamespace(content=[block], stop_reason=self.stop_reason, model=kwargs["model"])


def test_grounded_narrative_passes():
    c = FakeClient({"narrative": "T2 read stale balance at step 17, T1 wrote at step 27.", "cited_steps": [17, 27]})
    n = narrate(EVIDENCE, client=c)
    assert n.grounded and n.unknown_steps == []
    assert c.kwargs["model"] == "claude-opus-5-5"
    assert c.kwargs["output_config"]["format"]["type"] == "json_schema"


def test_invented_steps_are_flagged_even_in_prose():
    ok, unknown = grounded(EVIDENCE, [17], "it went wrong at step 4242")
    assert not ok and unknown == [4242]


def test_refusal_raises():
    c = FakeClient({"narrative": "", "cited_steps": []}, stop_reason="refusal")
    try:
        narrate(EVIDENCE, client=c)
    except RuntimeError:
        return
    raise AssertionError("expected a refusal error")
