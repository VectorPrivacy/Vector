"""MDK Python bindings - Marmot Development Kit"""
from .version import __version__

try:
    from .mdk_uniffi import *  # noqa: F403
except ImportError as e:
    from pathlib import Path
    import sys

    bindings_dir = Path(__file__).parent
    print("Error: Could not import mdk_uniffi. Make sure the bindings are built.", file=sys.stderr)
    print(f"Expected bindings at: {bindings_dir}", file=sys.stderr)
    print(f"Import error: {e}", file=sys.stderr)
    raise

__all__ = ["__version__"]

