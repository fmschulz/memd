"""memd-wiki: deterministic compiled markdown surface over memd."""

__version__ = "1.7.1"

__all__ = ["__version__", "build_wiki"]

from .compiler import build_wiki
