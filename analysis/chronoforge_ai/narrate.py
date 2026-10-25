from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any

MODEL = "claude-opus-5-5"

SYSTEM = """You write incident notes for engineers debugging a recorded program run.
You are given the program (with line numbers) and ChronoForge's analysis: the failure,
ranked hypotheses with evidence, data races, lock cycles and a backward dynamic slice.

Rules:
- Explain the single most likely root cause in plain language, then the fix.
- Every factual claim about execution must cite a step number from the evidence, written as "step 123".
- Only use step numbers that appear in the evidence. Never invent steps, lines or values.
- If the evidence is inconclusive, say so instead of guessing.
- Keep it under 180 words. Markdown bold for the two section leads is fine; no headings."""

SCHEMA = {
    "type": "object",
    "properties": {
        "narrative": {"type": "string"},
        "cited_steps": {"type": "array", "items": {"type": "integer"}},
    },
    "required": ["narrative", "cited_steps"],
    "additionalProperties": False,
}


@dataclass
class Narrative:
    narrative: str
    cited_steps: list[int]
    model: str
    grounded: bool
    unknown_steps: list[int] = field(default_factory=list)

    def to_json(self) -> str:
        return json.dumps(self.__dict__)


def known_steps(evidence: Any) -> set[int]:
    """Every value stored under a "step" key anywhere in the bundle."""
    out: set[int] = set()

    def walk(v: Any) -> None:
        if isinstance(v, dict):
            for k, x in v.items():
                if k == "step" and isinstance(x, int):
                    out.add(x)
                walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)

    walk(evidence)
    return out


def mentioned_steps(text: str) -> set[int]:
    return {int(m) for m in re.findall(r"\bsteps?\s+#?(\d+)", text, flags=re.I)}


def grounded(evidence: Any, cited: list[int], text: str) -> tuple[bool, list[int]]:
    """A narrative is grounded when every step it cites, in the list *and* in
    the prose, is one the engine actually reported."""
    known = known_steps(evidence)
    unknown = sorted((set(cited) | mentioned_steps(text)) - known)
    return not unknown, unknown


def narrate(evidence: dict[str, Any], client: Any = None) -> Narrative:
    if client is None:
        import anthropic

        client = anthropic.Anthropic()

    response = client.beta.messages.create(
        model=MODEL,
        max_tokens=4000,
        system=SYSTEM,
        # if the request is declined, let the API route it to a fallback model
        betas=["server-side-fallback-2026-07-01"],
        fallbacks="default",
        output_config={"effort": "medium", "format": {"type": "json_schema", "schema": SCHEMA}},
        messages=[{"role": "user", "content": json.dumps(evidence, indent=1)}],
    )
    if response.stop_reason == "refusal":
        raise RuntimeError("the model declined to write this narrative")
    text = next(b.text for b in response.content if b.type == "text")
    data = json.loads(text)
    ok, unknown = grounded(evidence, data["cited_steps"], data["narrative"])
    return Narrative(
        narrative=data["narrative"],
        cited_steps=data["cited_steps"],
        model=getattr(response, "model", MODEL),
        grounded=ok,
        unknown_steps=unknown,
    )
