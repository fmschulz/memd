"""memd-wiki: deterministic compiled markdown surface over memd."""

__version__ = "0.8.0"

__all__ = ["__version__", "build_wiki"]

from .compiler import build_wiki
