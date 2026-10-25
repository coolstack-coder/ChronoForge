"""python -m chronoforge_ai explain < evidence.json

Called by the ChronoForge server's /explain endpoint; also handy on its own:
    curl -s localhost:7420/api/sessions/<id>/analysis ... | python -m chronoforge_ai explain
"""

import json
import sys

from .narrate import narrate


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] != "explain":
        print(__doc__, file=sys.stderr)
        return 2
    evidence = json.load(sys.stdin)
    print(narrate(evidence).to_json())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
