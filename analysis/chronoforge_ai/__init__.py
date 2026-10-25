"""Turn ChronoForge's evidence bundle into a short incident write-up.

The Rust engine does the reasoning (races, slices, hypotheses). The model
only narrates it, and we refuse any narrative that cites steps the engine
never produced.
"""

from .narrate import MODEL, Narrative, grounded, narrate

__all__ = ["MODEL", "Narrative", "grounded", "narrate"]
